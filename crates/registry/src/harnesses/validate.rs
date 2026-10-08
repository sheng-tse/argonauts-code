//! Checks for one `[[harness]]` entry, line-based or an ACP profile.

use std::path::{Component, Path, PathBuf};

use argonauts_core::adapter::Transport;
use argonauts_core::registry::HarnessManifest;
use argonauts_core::registry::acp::{AcpProfile, ModelRoute, QuotaRoute};
use argonauts_core::registry::harness::{OutputKind, PromptChannel};
use regex::Regex;

use super::refusals::{
    OPENCLAW_ACP, arg_refusal, env_name_refusal, env_set_refusal, launcher_refusal, program_name,
    runs_openclaw_acp,
};
use super::wrappers::follow;

/// The placeholders a manifest may use. The manifest adapter keeps the same list.
pub const PLACEHOLDERS: &[&str] = &[
    "model",
    "effort",
    "workdir",
    "session",
    "home",
    "schema_file",
    "prompt_file",
    "prompt",
];

/// The built-in parsers `output = "parser"` can name.
pub const PARSERS: &[&str] = &["claude-code", "codex", "antigravity-cli", "opencode"];

/// Ids a manifest can't take, since they belong to the built-in adapters.
const ADAPTER_IDS: &[&str] = &["claude-code", "codex", "antigravity-cli", "opencode"];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ManifestCheck {
    /// Any error leaves the entry out.
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ManifestCheck {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }

    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

/// Refuses what could run something you didn't choose or weaken approvals: a `binary` that
/// isn't absolute or sits inside the repo, or a wrapper such as `env` or `sh` that runs one,
/// `env_set` names that look like secrets, switch approvals off, or load code, `{prompt}`
/// without `prompt = "argument"`, `openclaw acp`, `npx` and `uvx` launches, and known bypass
/// flags. Warns about `prompt = "argument"`, since any of your processes can read a command
/// line. `agents.<id>.binary` replaces `binary` before this runs.
pub fn check_manifest(manifest: &HarnessManifest, repo_root: Option<&Path>) -> ManifestCheck {
    check(manifest, Some(repo_root))
}

/// The same checks for an entry built into Argonauts, whose `binary` may be a bare program
/// found on `PATH`, as the built-in adapters' are.
pub fn check_builtin_manifest(manifest: &HarnessManifest) -> ManifestCheck {
    check(manifest, None)
}

/// `from_file` is `Some(repo_root)` for an entry read from a `models.toml`.
fn check(manifest: &HarnessManifest, from_file: Option<Option<&Path>>) -> ManifestCheck {
    let mut check = ManifestCheck::default();
    check_identity(manifest, &mut check);
    check_program(manifest, from_file, &mut check);
    check_environment(manifest, &mut check);
    check_arguments(manifest, &mut check);
    check_prompt(manifest, &mut check);
    check_output(manifest, &mut check);
    check_errors(manifest, &mut check);
    match (&manifest.acp, manifest.transport) {
        (Some(acp), Transport::Acp) => check_acp(manifest, acp, &mut check),
        (None, Transport::Acp) => check.error("transport = \"acp\" needs a [harness.acp] table"),
        (Some(_), Transport::Lines) => {
            check.error("a [harness.acp] table needs transport = \"acp\"")
        }
        (None, Transport::Lines) => {}
    }
    check
}

fn check_identity(manifest: &HarnessManifest, check: &mut ManifestCheck) {
    let id = &manifest.id;
    let plain = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        && !id.starts_with('.');
    if !plain {
        check.error(format!(
            "id {id:?} must be letters, digits, dashes, dots, and underscores"
        ));
    }
    if ADAPTER_IDS.contains(&id.as_str()) {
        check.error(format!(
            "id {id} belongs to a built-in adapter. Pick another"
        ));
    }
    if manifest.family.trim().is_empty() {
        check.error("provider is empty. Name the provider, or \"from-model\"");
    }
}

fn check_program(
    manifest: &HarnessManifest,
    from_file: Option<Option<&Path>>,
    check: &mut ManifestCheck,
) {
    let binary = manifest.binary.as_str();
    if binary.trim().is_empty() {
        check.error("binary is empty");
        return;
    }
    if let Some(repo_root) = from_file {
        if !Path::new(binary).is_absolute() {
            check.error(format!(
                "binary {binary:?} must be an absolute path, so what runs doesn't depend on PATH"
            ));
        } else if let Some(root) = repo_root
            && inside(Path::new(binary), root)
        {
            check.error(format!(
                "binary {binary} is inside the repo, where a commit could change what runs"
            ));
        }
    }
    let mut errors = Vec::new();
    for args in launches(manifest) {
        launch_errors(binary, args, from_file, "", &mut errors);
    }
    if let Some(probe) = manifest.acp.as_ref().map(|acp| &acp.quota_probe)
        && let Some((program, args)) = probe.split_first()
    {
        launch_errors(program, args, from_file, "acp.quota_probe: ", &mut errors);
    }
    for error in errors {
        check.error(error);
    }
}

/// The argument lists that start `binary`. The role lists always do; the others only when set.
fn launches(manifest: &HarnessManifest) -> impl Iterator<Item = &[String]> {
    let optional = [
        &manifest.resume_args,
        &manifest.interactive_resume,
        &manifest.version_args,
    ];
    [&manifest.read_only_args, &manifest.build_args]
        .into_iter()
        .chain(optional.into_iter().filter(|args| !args.is_empty()))
        .map(Vec::as_slice)
}

/// Adds what's wrong with one launch to `errors`, once each. A program a wrapper runs, such
/// as the script in `/bin/sh run.sh`, is held to the rule for `binary` when the entry is from
/// a file, since the working folder is the card's worktree.
fn launch_errors(
    program: &str,
    args: &[String],
    from_file: Option<Option<&Path>>,
    prefix: &str,
    errors: &mut Vec<String>,
) {
    let chain = follow(program, args);
    let mut found = chain.refusals;
    found.extend(launcher_refusal(program, args));
    if runs_openclaw_acp(program, args) {
        found.push(OPENCLAW_ACP.to_owned());
    }
    if let Some(repo_root) = from_file {
        for step in &chain.steps {
            let Some(wrapper) = step.wrapper else {
                continue;
            };
            let wrapped = step.program;
            if !Path::new(wrapped).is_absolute() {
                found.push(format!(
                    "{wrapper} runs {wrapped:?}, which must be an absolute path, so what runs \
                     doesn't depend on PATH or the working folder"
                ));
            } else if let Some(root) = repo_root
                && inside(Path::new(wrapped), root)
            {
                found.push(format!(
                    "{wrapper} runs {wrapped}, which is inside the repo, where a commit could \
                     change what runs"
                ));
            }
        }
    }
    for why in found {
        let error = format!("{prefix}{why}");
        if !errors.contains(&error) {
            errors.push(error);
        }
    }
}

fn check_environment(manifest: &HarnessManifest, check: &mut ManifestCheck) {
    for name in manifest.env_set.keys() {
        if !is_variable_name(name) {
            check.error(format!("env_set {name:?} isn't a variable name"));
        } else if let Some(why) = env_set_refusal(name) {
            check.error(why);
        }
    }
    if let Some(home) = &manifest.home {
        if !is_variable_name(&home.env) {
            check.error(format!("home.env {:?} isn't a variable name", home.env));
        } else if let Some(why) = env_name_refusal(&home.env) {
            check.error(why);
        }
        let name = home.template.as_str();
        let mut parts = Path::new(name).components();
        let one_folder = matches!(
            (parts.next(), parts.next()),
            (Some(Component::Normal(_)), None)
        );
        if !one_folder || name.contains(['/', '\\', '\0']) || name.contains("..") {
            check.error(format!(
                "home.template {name:?} must be one folder name, without / or .."
            ));
        }
    }
}

fn is_variable_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every argument list an entry can launch with, named for messages.
fn argument_lists(manifest: &HarnessManifest) -> [(&'static str, &[String]); 6] {
    let quota_probe = manifest
        .acp
        .as_ref()
        .map_or(&[][..], |acp| &acp.quota_probe[..]);
    [
        ("read_only_args", &manifest.read_only_args),
        ("build_args", &manifest.build_args),
        ("resume_args", &manifest.resume_args),
        ("interactive_resume", &manifest.interactive_resume),
        ("version_args", &manifest.version_args),
        ("acp.quota_probe", quota_probe),
    ]
}

fn check_arguments(manifest: &HarnessManifest, check: &mut ManifestCheck) {
    let hermes = manifest.id == "hermes" || program_name(&manifest.binary) == "hermes";
    for (list, args) in argument_lists(manifest) {
        for arg in args {
            if let Some(why) = arg_refusal(arg, hermes) {
                check.error(format!("{list}: {why}"));
            }
            for name in placeholders_in(arg) {
                if !PLACEHOLDERS.contains(&name) {
                    check.error(format!(
                        "{list}: {{{name}}} isn't a placeholder Argonauts knows"
                    ));
                } else if name == "session" && !matches!(list, "resume_args" | "interactive_resume")
                {
                    check.error(format!(
                        "{list}: {{session}} is filled only in resume_args and interactive_resume"
                    ));
                } else if list == "interactive_resume" && !matches!(name, "session" | "workdir") {
                    check.error(format!(
                        "interactive_resume gets only {{session}} and {{workdir}}, not {{{name}}}"
                    ));
                } else if name == "home" && manifest.home.is_none() {
                    check.warn(format!(
                        "{list}: {{home}} points at a folder no template fills, since there's no \
                         `home`"
                    ));
                }
            }
        }
    }
}

/// The names of the `{name}` placeholders in `arg`. A name is ASCII letters, digits, and
/// underscores, so JSON in an argument isn't mistaken for one.
pub fn placeholders_in(arg: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = arg;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        if end > 0 && after[end..].starts_with('}') {
            names.push(&after[..end]);
            rest = &after[end + 1..];
        } else {
            rest = after;
        }
    }
    names
}

fn uses(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| placeholders_in(arg).contains(&name))
}

fn check_prompt(manifest: &HarnessManifest, check: &mut ManifestCheck) {
    let role_lists = [&manifest.read_only_args, &manifest.build_args];
    let argument = manifest.prompt == PromptChannel::Argument;
    if !argument
        && argument_lists(manifest)
            .iter()
            .any(|(_, args)| uses(args, "prompt"))
    {
        check.error("{prompt} is allowed only with prompt = \"argument\"");
    }
    if manifest.transport == Transport::Acp {
        return;
    }
    match manifest.prompt {
        PromptChannel::Stdin => {}
        PromptChannel::Argument => {
            check.warn(
                "prompt = \"argument\" puts the prompt on the command line, where any of your \
                 processes can read it. Use stdin or a file if the CLI has either",
            );
            if role_lists.iter().any(|args| !uses(args, "prompt")) {
                check
                    .error("prompt = \"argument\" needs {prompt} in read_only_args and build_args");
            }
        }
        PromptChannel::File => {
            if role_lists.iter().any(|args| !uses(args, "prompt_file")) {
                check.error(
                    "prompt = \"file\" needs {prompt_file} in read_only_args and build_args",
                );
            }
        }
    }
}

fn check_output(manifest: &HarnessManifest, check: &mut ManifestCheck) {
    if manifest.transport == Transport::Acp {
        if manifest.output != OutputKind::default() || manifest.parser.is_some() {
            check.warn("output and parser are ignored for an ACP profile");
        }
        return;
    }
    match (manifest.output, manifest.parser.as_deref()) {
        (OutputKind::Parser, Some(name)) if PARSERS.contains(&name) => {}
        (OutputKind::Parser, Some(name)) => check.error(format!(
            "parser {name:?} isn't a built-in parser. Use one of {}",
            PARSERS.join(", ")
        )),
        (OutputKind::Parser, None) => check.error(format!(
            "output = \"parser\" needs parser, one of {}",
            PARSERS.join(", ")
        )),
        (_, Some(_)) => check.warn("parser is ignored unless output = \"parser\""),
        (_, None) => {}
    }
    let final_json = manifest.output == OutputKind::FinalJson;
    match &manifest.final_json {
        None if final_json => {
            check.error("output = \"final-json\" needs a [harness.final_json] table")
        }
        None => {}
        Some(_) if !final_json => {
            check.warn("[harness.final_json] is ignored unless output = \"final-json\"")
        }
        Some(pointers) => {
            let named = [
                ("text", &pointers.text),
                ("status", &pointers.status),
                ("session_id", &pointers.session_id),
                ("model", &pointers.model),
                ("input_tokens", &pointers.input_tokens),
                ("output_tokens", &pointers.output_tokens),
                ("cached_tokens", &pointers.cached_tokens),
                ("cost_usd", &pointers.cost_usd),
                ("error_kind", &pointers.error_kind),
                ("error_message", &pointers.error_message),
            ];
            if named.iter().all(|(_, pointer)| pointer.is_none()) {
                check.error("[harness.final_json] names no pointers");
            }
            for (key, pointer) in named {
                if let Some(pointer) = pointer
                    && !pointer.is_empty()
                    && !pointer.starts_with('/')
                {
                    check.error(format!(
                        "final_json.{key} {pointer:?} isn't a JSON pointer, which starts with /"
                    ));
                }
            }
            if pointers.status.is_some() && pointers.ok_status.is_empty() {
                check.warn("final_json.status has no ok_status, so every status counts as success");
            }
        }
    }
    if let Some(schema) = &manifest.schema
        && (schema.flag.trim().is_empty() || schema.field.trim().is_empty())
    {
        check.error("schema needs both flag and field");
    }
    if let Some(stop) = &manifest.stop {
        for (key, span) in [
            ("after_interrupt", &stop.after_interrupt),
            ("after_terminate", &stop.after_terminate),
        ] {
            if let Err(why) = span.parse() {
                check.error(format!("stop.{key}: {why}"));
            }
        }
    }
}

fn check_errors(manifest: &HarnessManifest, check: &mut ManifestCheck) {
    for rule in &manifest.errors {
        if let Err(err) = Regex::new(&rule.pattern) {
            check.error(format!(
                "error pattern {:?} doesn't compile: {err}",
                rule.pattern
            ));
        }
    }
    for code in manifest.exit_codes.keys() {
        if code.trim().parse::<i32>().is_err() {
            check.error(format!("exit code {code:?} isn't a number"));
        }
    }
}

fn check_acp(manifest: &HarnessManifest, acp: &AcpProfile, check: &mut ManifestCheck) {
    match (&acp.quota, acp.quota_probe.is_empty()) {
        (QuotaRoute::Probe, true) => check.error("quota = \"probe\" needs quota_probe"),
        (QuotaRoute::Probe, false) => {}
        (_, false) => check.warn("quota_probe is ignored unless quota = \"probe\""),
        (_, true) => {}
    }
    if let QuotaRoute::SameLoginAs(other) = &acp.quota
        && *other == manifest.id
    {
        check.error("plan usage can't count against the profile's own login");
    }
    let role_lists = [&manifest.read_only_args, &manifest.build_args];
    if acp.model == ModelRoute::LaunchArg && role_lists.iter().any(|args| !uses(args, "model")) {
        check.error("model = \"launch-arg\" needs {model} in read_only_args and build_args");
    }
    if acp.modes.build.is_none() {
        check.warn("[harness.acp] has no build mode, so this agent can't own a task");
    }
    if acp.usage.is_empty() {
        check.warn("[harness.acp] reads no usage, so spend on this agent isn't counted");
    }
}

/// Whether `path` is under `root`, by its text and by where symlinks lead, so neither
/// `/repo/../repo/bin/x` nor a link into the repo passes.
fn inside(path: &Path, root: &Path) -> bool {
    normalize(path).starts_with(normalize(root)) || resolve(path).starts_with(resolve(root))
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The real path of the deepest part that exists, with the rest joined on, so a binary not
/// installed yet still compares against a root reached through a symlink (`/tmp` on macOS).
fn resolve(path: &Path) -> PathBuf {
    let path = normalize(path);
    let mut rest = Vec::new();
    let mut base = path.as_path();
    loop {
        if let Ok(real) = base.canonicalize() {
            return rest.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                base = parent;
            }
            _ => return path,
        }
    }
}
