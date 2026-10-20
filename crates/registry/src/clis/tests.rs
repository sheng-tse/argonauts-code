use std::path::Path;

use argonauts_core::adapter::Transport;
use argonauts_core::registry::acp::{
    EffortRoute, ModelRoute, PlanUsageRoute, ReportRoute, ResumeRoute, SteerRoute, UsageSource,
};
use argonauts_core::registry::cli::{HomeKeep, PromptChannel};
use argonauts_core::registry::{CliManifest, RegistryFile};
use serde_json::{Value, json};

use super::builtin::builtin_files;
use super::*;

fn parse(toml_text: &str) -> Result<CliManifest, String> {
    let file: RegistryFile = toml::from_str(toml_text).map_err(|err| err.to_string())?;
    file.clis
        .into_iter()
        .next()
        .ok_or_else(|| "no [[cli]] entry".to_owned())
}

fn errors_of(manifest: &CliManifest) -> String {
    check_manifest(manifest, Some(Path::new("/work/repo")))
        .errors
        .join("\n")
}

const PROFILE: &str = r#"
[[cli]]
id = "my-acp-agent"
display_name = "My ACP Agent"
binary = "/opt/my-acp-agent/bin/agent"
read_only_args = ["acp"]
build_args = ["acp"]
provider = "from-model"
transport = "acp"
errors = [{ pattern = "quota exceeded", category = "usage_limit" }]

[cli.acp]
modes = { read_only = "plan", build = "default" }
model = "legacy-set-model"
effort = "config-option"
report = "final-prompt"
resume = "load"
steer = "input"
usage = ["end-turn", "usage-update"]
plan_usage = "same-login-as:codex"
mcp_servers = false
"#;

fn profile() -> CliManifest {
    parse(PROFILE).unwrap()
}

#[test]
fn acp_profile_parses_and_validates() {
    let manifest = profile();
    assert_eq!(manifest.transport, Transport::Acp);
    let acp = manifest.acp.clone().unwrap();
    assert_eq!(acp.modes.read_only.as_deref(), Some("plan"));
    assert_eq!(acp.modes.build.as_deref(), Some("default"));
    assert_eq!(acp.modes.experiment, None);
    assert_eq!(acp.model, ModelRoute::LegacySetModel);
    assert_eq!(acp.effort, EffortRoute::ConfigOption);
    assert_eq!(acp.report, ReportRoute::FinalPrompt);
    assert_eq!(acp.resume, ResumeRoute::Load);
    assert_eq!(acp.steer, SteerRoute::Input);
    assert_eq!(acp.usage, [UsageSource::EndTurn, UsageSource::UsageUpdate]);
    assert_eq!(acp.plan_usage, PlanUsageRoute::SameLoginAs("codex".into()));
    assert!(!acp.mcp_servers);
    let check = check_manifest(&manifest, Some(Path::new("/work/repo")));
    assert!(check.is_ok(), "{check:?}");
    assert!(check.warnings.is_empty(), "{check:?}");

    // A CLI-level key inside [cli.acp] fails to load instead of being lost.
    let misplaced = PROFILE.replace("mcp_servers = false", "interactive_resume = []");
    assert!(
        parse(&misplaced)
            .unwrap_err()
            .contains("interactive_resume")
    );
    let bad_route = PROFILE.replace("\"legacy-set-model\"", "\"guess\"");
    assert!(parse(&bad_route).is_err());

    // The transport and the table come together.
    let no_table = CliManifest {
        acp: None,
        ..profile()
    };
    assert!(errors_of(&no_table).contains("needs a [cli.acp] table"));
    let no_transport = CliManifest {
        transport: Transport::Lines,
        ..profile()
    };
    assert!(errors_of(&no_transport).contains("needs transport = \"acp\""));

    // OpenClaw's bridge, by name, with the reason.
    let openclaw = CliManifest {
        binary: "/opt/homebrew/bin/openclaw".into(),
        ..profile()
    };
    assert!(errors_of(&openclaw).contains("runs tools in the Gateway"));
    for program in ["openclaw", "/opt/homebrew/bin/openclaw"] {
        let wrapped = CliManifest {
            binary: "/usr/bin/env".into(),
            read_only_args: vec![program.into(), "acp".into()],
            build_args: vec![program.into(), "acp".into()],
            ..profile()
        };
        assert!(
            errors_of(&wrapped).contains("runs tools in the Gateway"),
            "{program}"
        );
    }

    // Launchers that fetch the agent each time.
    for (binary, args) in [
        ("/usr/local/bin/npx", vec!["-y", "some-acp-agent"]),
        ("/opt/homebrew/bin/uvx", vec!["some-acp-agent", "acp"]),
        ("/usr/bin/env", vec!["npx", "-y", "some-acp-agent"]),
        (
            "/usr/bin/env",
            vec!["-i", "/usr/local/bin/npx", "-y", "some-acp-agent"],
        ),
        (
            "/usr/bin/nohup",
            vec!["/usr/bin/env", "npm", "exec", "some-acp-agent"],
        ),
        (
            "/usr/local/bin/node",
            vec!["/usr/local/bin/npx", "some-acp-agent"],
        ),
    ] {
        let fetched = CliManifest {
            binary: binary.into(),
            read_only_args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            build_args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            ..profile()
        };
        assert!(
            errors_of(&fetched).contains("downloads and runs a package"),
            "{binary}"
        );
    }

    // A wrapper's program is held to binary's rule, in every list that launches it, since the
    // working folder is the task's worktree.
    let wrapped = |binary: &str, args: &[&str]| CliManifest {
        binary: binary.into(),
        read_only_args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        build_args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        ..profile()
    };
    for (binary, args, want) in [
        (
            "/bin/sh",
            &["./agent.sh", "acp"][..],
            "sh runs \"./agent.sh\", which must be an absolute",
        ),
        (
            "/usr/bin/env",
            &["agent", "acp"],
            "env runs \"agent\", which must be an absolute",
        ),
        (
            "/usr/bin/python3",
            &["/work/repo/agent.py", "acp"],
            "which is inside the repo",
        ),
        (
            "/bin/bash",
            &["-c", "/opt/agent acp"],
            "bash -c comes before the script",
        ),
        (
            "/usr/bin/node",
            &["--require", "./x.js", "/opt/a.js"],
            "comes before the script",
        ),
        ("/usr/bin/env", &["-S", "/usr/local/bin/npx -y x"], "env -S"),
        (
            "/usr/bin/env",
            &["NODE_OPTIONS=--require=./x.js", "/opt/a"],
            "Put it in env_set",
        ),
        (
            "/usr/bin/nohup",
            &["/opt/agent acp"],
            "a command line no check can see into",
        ),
    ] {
        let errors = errors_of(&wrapped(binary, args));
        assert!(errors.contains(want), "{binary} {args:?}: {errors}");
    }
    assert_eq!(
        errors_of(&wrapped("/usr/bin/env", &["/opt/a/agent", "acp"])),
        ""
    );
    let resumed = CliManifest {
        resume_args: vec!["npx".into(), "some-acp-agent".into()],
        ..wrapped("/usr/bin/env", &["/opt/a/agent", "acp"])
    };
    assert!(errors_of(&resumed).contains("downloads and runs a package"));

    // Bypass switches, as variables and as flags; Hermes' short flags only for Hermes.
    let mut yolo = profile();
    yolo.env_set.insert("HERMES_YOLO_MODE".into(), "1".into());
    yolo.env_set
        .insert("HERMES_KANBAN_BOARD".into(), "x".into());
    let errors = errors_of(&yolo);
    assert!(
        errors.contains("env_set HERMES_YOLO_MODE switches"),
        "{errors}"
    );
    assert!(
        errors.contains("env_set HERMES_KANBAN_BOARD switches"),
        "{errors}"
    );
    for flag in ["--yolo", "--accept-hooks", "-z", "-w"] {
        let hermes = CliManifest {
            id: "hermes".into(),
            build_args: vec!["acp".into(), flag.into()],
            ..profile()
        };
        assert!(
            errors_of(&hermes).contains(&format!("{flag} switches")),
            "{flag}"
        );
    }
    let other = CliManifest {
        build_args: vec!["acp".into(), "-w".into()],
        ..profile()
    };
    assert_eq!(errors_of(&other), "");

    // Routes that need something the profile doesn't give.
    let mut probe = profile();
    if let Some(acp) = &mut probe.acp {
        acp.plan_usage = PlanUsageRoute::Probe;
    }
    assert!(errors_of(&probe).contains("needs plan_usage_probe"));
    if let Some(acp) = &mut probe.acp {
        acp.plan_usage_probe = vec!["/usr/local/bin/npx".into(), "usage".into()];
    }
    assert!(errors_of(&probe).contains("acp.plan_usage_probe"));
    let mut launch_arg = profile();
    if let Some(acp) = &mut launch_arg.acp {
        acp.model = ModelRoute::LaunchArg;
    }
    assert!(errors_of(&launch_arg).contains("needs {model}"));
    launch_arg
        .read_only_args
        .extend(["--model".into(), "{model}".into()]);
    launch_arg
        .build_args
        .extend(["--model".into(), "{model}".into()]);
    assert_eq!(errors_of(&launch_arg), "");

    let mut no_build = profile();
    if let Some(acp) = &mut no_build.acp {
        acp.modes.build = None;
    }
    let check = check_manifest(&no_build, None);
    assert!(check.is_ok());
    assert!(check.warnings[0].contains("can't own a task"));

    let mut bad_regex = profile();
    bad_regex.errors[0].pattern = "(unclosed".into();
    assert!(errors_of(&bad_regex).contains("doesn't compile"));
}

#[test]
fn builtin_profiles_pass_their_checks() {
    for (name, parsed) in builtin_files() {
        let manifests = parsed.unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(manifests.len(), 1, "{name}");
        for manifest in manifests {
            let check = check_builtin_manifest(&manifest);
            assert!(check.is_ok(), "{name}: {check:?}");
            assert!(check.warnings.is_empty(), "{name}: {check:?}");
            assert_eq!(manifest.provider, "from-model", "{name}");
            assert_eq!(manifest.transport, Transport::Acp, "{name}");
            // Bare programs are fine built in, but not in your file.
            assert!(!check_manifest(&manifest, None).is_ok(), "{name}");
        }
    }
    let ids = |turned_on: &[String]| -> Vec<String> {
        builtin_clis(turned_on).into_iter().map(|m| m.id).collect()
    };
    // Hermes is off until you turn it on.
    assert_eq!(ids(&[]), ["opencode-acp"]);
    let hermes_on = ["hermes".to_owned()];
    assert_eq!(ids(&hermes_on), ["opencode-acp", "hermes"]);
    let hermes = builtin_clis(&hermes_on).pop().unwrap();
    assert_eq!(hermes.read_only_args, ["acp"]);
    assert_eq!(
        hermes.acp.as_ref().map(|acp| acp.model),
        Some(ModelRoute::LegacySetModel)
    );
}

// What OpenCode 1.18.34 did over ACP: a live session counted no spend while the profile waited
// for usage_update, which it never sends.
#[test]
fn opencode_acp_matches_what_it_did() {
    let opencode = builtin_clis(&[]).pop().unwrap();
    // `opencode acp` talks to the server it starts over HTTP on loopback.
    assert!(opencode.own_server);
    let acp = opencode.acp.unwrap();
    assert_eq!(acp.usage, [UsageSource::EndTurn]);
    assert_eq!(acp.resume, ResumeRoute::Resume);
    assert_eq!(acp.model, ModelRoute::ConfigOption);
    assert_eq!(acp.modes.read_only.as_deref(), Some("plan"));
    assert_eq!(acp.modes.build.as_deref(), Some("build"));
}

#[test]
fn manifest_env_set_refuses_secrets_and_bypass() {
    for name in [
        "OPENAI_API_KEY",
        "GH_TOKEN",
        "my_token",
        "DB_PASSWORD",
        "CLIENT_SECRET",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "HERMES_YOLO_MODE",
        "HERMES_ACCEPT_HOOKS",
        "HERMES_KANBAN_TOKENLESS",
        // Code the agent or its tools load, or the program that runs.
        "LD_PRELOAD",
        "DYLD_INSERT_LIBRARIES",
        "NODE_OPTIONS",
        "PYTHONPATH",
        "PERL5OPT",
        "BASH_ENV",
        "PATH",
        "HOME",
        "GIT_CONFIG_GLOBAL",
    ] {
        let mut manifest = line_based();
        manifest.env_set.insert(name.into(), "x".into());
        let errors = errors_of(&manifest);
        assert!(
            errors.contains(&format!("env_set {name}")),
            "{name}: {errors}"
        );
    }
    let mut manifest = line_based();
    manifest
        .env_set
        .insert("GROK_DISABLE_AUTOUPDATER".into(), "1".into());
    manifest
        .env_set
        .insert("TOKENIZER_MODE".into(), "fast".into());
    assert_eq!(errors_of(&manifest), "");

    // The home's variable too, and names that aren't names.
    let mut manifest = line_based();
    manifest.home = Some(argonauts_core::registry::cli::HomeTemplate {
        env: "AGENT_TOKEN".into(),
        template: "../elsewhere".into(),
        keep: HomeKeep::Session,
    });
    manifest.env_set.insert("A=B".into(), "1".into());
    let errors = errors_of(&manifest);
    assert!(
        errors.contains("env_set AGENT_TOKEN looks like a secret"),
        "{errors}"
    );
    assert!(errors.contains("one folder name"), "{errors}");
    assert!(errors.contains("\"A=B\" isn't a variable name"), "{errors}");

    // The home's variable may be HOME, since Argonauts Code fills it with a folder it made.
    let mut manifest = line_based();
    manifest.home = Some(argonauts_core::registry::cli::HomeTemplate {
        env: "HOME".into(),
        template: "agent".into(),
        keep: HomeKeep::Session,
    });
    assert_eq!(errors_of(&manifest), "");
}

fn line_based() -> CliManifest {
    parse(
        r#"
[[cli]]
id = "my-agent"
display_name = "My Agent"
binary = "/opt/my-agent/bin/agent"
read_only_args = ["--read-only", "--model", "{model}"]
build_args = ["--model", "{model}", "--cwd", "{workdir}"]
resume_args = ["--resume", "{session}"]
provider = "mine"
"#,
    )
    .unwrap()
}

fn entry(id: &str, binary: &str, provider: &str) -> String {
    format!(
        "[[cli]]\nid = \"{id}\"\ndisplay_name = \"{id}\"\nbinary = \"{binary}\"\n\
         read_only_args = []\nbuild_args = []\nprovider = \"{provider}\"\n\n"
    )
}

#[cfg(unix)]
#[test]
fn project_cli_entries_need_trust() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".argonauts")).unwrap();
    let global = dir.path().join("models.toml");
    let project = repo.join(".argonauts/models.toml");
    std::fs::write(
        &global,
        entry("shared", "/opt/shared/bin/a", "global") + &entry("mine", "/opt/mine/bin/a", "x"),
    )
    .unwrap();
    let inside = repo.join("bin/agent").to_string_lossy().into_owned();
    std::fs::write(
        &project,
        entry("shared", "/opt/shared/bin/a", "project")
            + &entry("theirs", "/opt/theirs/bin/a", "y")
            + &entry("planted", &inside, "z")
            + &entry("bare", "agent", "z"),
    )
    .unwrap();
    let ids = |entries: &CliEntries| -> Vec<String> {
        entries.manifests.iter().map(|m| m.id.clone()).collect()
    };

    let untrusted = cli_manifests(&global, Some(&project), false, Some(&repo), &[]);
    assert_eq!(ids(&untrusted), ["opencode-acp", "shared", "mine"]);
    assert_eq!(untrusted.manifests[1].provider, "global");
    assert_eq!(untrusted.warnings.len(), 1, "{:?}", untrusted.warnings);
    let warning = &untrusted.warnings[0];
    assert!(
        warning.starts_with(&project.display().to_string()),
        "{warning}"
    );
    assert!(
        warning.contains("its 4 CLI entries are ignored until you trust"),
        "{warning}"
    );

    let trusted = cli_manifests(&global, Some(&project), true, Some(&repo), &[]);
    assert_eq!(ids(&trusted), ["opencode-acp", "shared", "mine", "theirs"]);
    // Later wins by id.
    assert_eq!(trusted.manifests[1].provider, "project");
    // Even trusted, a binary in the repo or on PATH is left out, naming the file and the id.
    assert_eq!(trusted.warnings.len(), 2, "{:?}", trusted.warnings);
    let planted = &trusted.warnings[0];
    assert!(
        planted.starts_with(&project.display().to_string()),
        "{planted}"
    );
    assert!(planted.contains("CLI planted is left out"), "{planted}");
    assert!(planted.contains("inside the repo"), "{planted}");
    assert!(
        trusted.warnings[1]
            .contains("CLI bare is left out: binary \"agent\" must be an absolute path")
    );

    // A link into the repo is inside it too.
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&repo, &link).unwrap();
    let linked = CliManifest {
        binary: link.join("bin/agent").to_string_lossy().into_owned(),
        ..line_based()
    };
    assert!(errors_of_in(&linked, &repo).contains("inside the repo"));
    let dotted = CliManifest {
        binary: format!("{}/../repo/bin/agent", repo.display()),
        ..line_based()
    };
    assert!(errors_of_in(&dotted, &repo).contains("inside the repo"));

    // No files at all: just the built-ins.
    let none = cli_manifests(&dir.path().join("missing.toml"), None, false, None, &[]);
    assert_eq!(ids(&none), ["opencode-acp"]);
    assert!(none.warnings.is_empty());
}

fn errors_of_in(manifest: &CliManifest, repo: &Path) -> String {
    check_manifest(manifest, Some(repo)).errors.join("\n")
}

#[test]
fn broken_entries_are_reported_by_file_and_id() {
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("models.toml");
    std::fs::write(
        &global,
        entry("fine", "/opt/fine/bin/a", "x")
            + "[[cli]]\nid = \"half\"\nbinary = \"/opt/half\"\n\n"
            + &entry("codex", "/opt/codex/bin/codex", "openai"),
    )
    .unwrap();
    let entries = cli_manifests(&global, None, false, None, &[]);
    assert_eq!(entries.manifests.len(), 2);
    let joined = entries.warnings.join("\n");
    assert!(
        joined.contains("CLI half is left out: missing field"),
        "{joined}"
    );
    assert!(
        joined.contains("CLI codex is left out: id codex belongs to a built-in adapter"),
        "{joined}"
    );

    std::fs::write(&global, "[[cli]\nbroken").unwrap();
    let entries = cli_manifests(&global, None, false, None, &[]);
    assert_eq!(entries.manifests.len(), 1);
    assert!(
        entries.warnings[0].contains("doesn't parse"),
        "{:?}",
        entries.warnings
    );
}

#[test]
fn hermes_is_off_until_turned_on() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let models = dir.path().join("models.toml");
    let manifests = |text: &str| -> Vec<String> {
        std::fs::write(&config, text).unwrap();
        let sources = crate::SettingsSources {
            global: Some(config.clone()),
            ..Default::default()
        };
        let resolved = crate::resolve(&sources).unwrap();
        let turned_on = agents_turned_on(&resolved);
        cli_manifests(&models, None, false, None, &turned_on)
            .manifests
            .into_iter()
            .map(|manifest| manifest.id)
            .collect()
    };
    assert_eq!(manifests(""), ["opencode-acp"]);
    // A table without `enabled`, or with it false, leaves it off too.
    assert_eq!(
        manifests("[agents.hermes]\nbinary = \"/opt/hermes/bin/hermes\"\n"),
        ["opencode-acp"]
    );
    assert_eq!(
        manifests("[agents.hermes]\nenabled = false\n"),
        ["opencode-acp"]
    );
    assert_eq!(
        manifests("[agents.hermes]\nenabled = true\n"),
        ["opencode-acp", "hermes"]
    );
}

#[test]
fn hermes_home_template_keys() {
    let hermes = builtin_clis(&["hermes".to_owned()])
        .into_iter()
        .find(|manifest| manifest.id == "hermes")
        .unwrap();
    let home = hermes.home.clone().unwrap();
    assert_eq!(home.env, "HERMES_HOME");
    assert_eq!(home.keep, HomeKeep::Task);
    let files = builtin_home_template(&home.template).unwrap();
    // No .env, which would override the environment Argonauts Code sets.
    let names: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
    assert_eq!(names, ["config.yaml"]);
    let config: Value = serde_saphyr::from_str(files[0].1).unwrap();
    let at = |pointer: &str| config.pointer(pointer).cloned();
    for (pointer, want) in [
        ("/fallback_providers", json!([])),
        ("/approvals/mode", json!("manual")),
        ("/auth/adopt_external_logins", json!(false)),
        ("/memory/memory_enabled", json!(false)),
        ("/memory/user_profile_enabled", json!(false)),
        ("/auxiliary/background_review/enabled", json!(false)),
        ("/skills/write_approval", json!(true)),
        ("/skills/project_discovery", json!(false)),
        ("/skills/external_dirs", json!([])),
        ("/security/allow_lazy_installs", json!(false)),
        ("/updates/check", json!(false)),
        ("/terminal/backend", json!("local")),
    ] {
        assert_eq!(at(pointer), Some(want), "{pointer}");
    }
    let wait = at("/terminal/oneshot_completion_wait_seconds").and_then(|v| v.as_u64());
    assert!(wait.is_some_and(|seconds| seconds <= 10), "{wait:?}");
    assert!(at("/delegation/max_concurrent_children").is_some());
    assert!(builtin_home_template("elsewhere").is_none());
}

#[test]
fn manifest_prompt_argument_warns() {
    let argument = CliManifest {
        prompt: PromptChannel::Argument,
        read_only_args: vec!["-p".into(), "{prompt}".into()],
        build_args: vec![
            "-p".into(),
            "{prompt}".into(),
            "--model".into(),
            "{model}".into(),
        ],
        ..line_based()
    };
    let check = check_manifest(&argument, None);
    assert!(check.is_ok(), "{check:?}");
    assert!(check.warnings[0].contains("any of your processes can read it"));

    // `{prompt}` needs the argument channel.
    let sneaky = CliManifest {
        read_only_args: vec!["-p".into(), "{prompt}".into()],
        ..line_based()
    };
    assert!(errors_of(&sneaky).contains("{prompt} is allowed only with prompt = \"argument\""));

    // A channel the arguments never use would lose the prompt.
    let unused = CliManifest {
        prompt: PromptChannel::Argument,
        ..line_based()
    };
    assert!(errors_of(&unused).contains("needs {prompt}"));
    let by_file = CliManifest {
        prompt: PromptChannel::File,
        ..line_based()
    };
    assert!(errors_of(&by_file).contains("needs {prompt_file}"));
}

#[test]
fn placeholders_and_outputs_are_checked() {
    let unknown = CliManifest {
        build_args: vec!["--dir".into(), "{worktree}".into(), r#"{"json":1}"#.into()],
        ..line_based()
    };
    assert_eq!(
        errors_of(&unknown),
        "build_args: {worktree} isn't a placeholder Argonauts Code knows"
    );
    let early_session = CliManifest {
        build_args: vec!["--session".into(), "{session}".into()],
        ..line_based()
    };
    assert!(errors_of(&early_session).contains("{session} is filled only in resume_args"));

    let parser = |name: Option<&str>| CliManifest {
        output: argonauts_core::registry::cli::OutputKind::Parser,
        parser: name.map(str::to_owned),
        ..line_based()
    };
    assert_eq!(errors_of(&parser(Some("claude-code"))), "");
    assert!(errors_of(&parser(Some("grok"))).contains("isn't a built-in parser"));
    assert!(errors_of(&parser(None)).contains("needs parser"));

    let final_json = CliManifest {
        output: argonauts_core::registry::cli::OutputKind::FinalJson,
        ..line_based()
    };
    assert!(errors_of(&final_json).contains("needs a [cli.final_json] table"));
    let empty = CliManifest {
        final_json: Some(Default::default()),
        ..final_json.clone()
    };
    assert!(errors_of(&empty).contains("names no pointers"));
    let not_a_pointer = CliManifest {
        final_json: Some(argonauts_core::registry::cli::FinalJsonPointers {
            text: Some("final".into()),
            ..Default::default()
        }),
        ..final_json
    };
    assert!(errors_of(&not_a_pointer).contains("final_json.text \"final\" isn't a JSON pointer"));

    let mut exit_codes = line_based();
    exit_codes
        .exit_codes
        .insert("two".into(), argonauts_core::ErrorCategory::Timeout);
    assert!(errors_of(&exit_codes).contains("exit code \"two\" isn't a number"));
}
