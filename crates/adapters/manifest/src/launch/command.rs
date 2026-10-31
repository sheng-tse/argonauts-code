//! The command line: `read_only_args` or `build_args` with `{model}`, `{effort}`,
//! `{workdir}`, `{home}`, `{schema_file}`, and `{prompt_file}` filled in, `resume_args` with
//! `{session}`, and the prompt on stdin, in a file, or as `{prompt}` where the manifest allows.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use argonauts_core::agents::adapter::Transport;
use argonauts_core::agents::session::{SessionRole, SpecFile, env_remove_except};
use argonauts_core::base::layout::{session_files, user_files};
use argonauts_core::base::paths::argonauts_home;
use argonauts_core::models::registry::CliManifest;
use argonauts_core::models::registry::cli::PromptChannel;
use argonauts_core::{CommandSpec, SessionRequest};

use crate::launch::home::home_files;
use crate::launch::placeholders::{Fill, fill_args, placeholders_in};

/// `templates_root` is where home templates live, `~/.argonauts/clis/` when `None`.
pub(crate) fn build(
    manifest: &CliManifest,
    req: &SessionRequest,
    templates_root: Option<&Path>,
) -> anyhow::Result<CommandSpec> {
    let id = &manifest.id;
    if manifest.transport == Transport::Acp {
        bail!("{id} is an ACP profile, which runs through the ACP adapter");
    }
    crate::output::ErrorRules::compile(manifest).with_context(|| format!("CLI {id}"))?;
    // A take-over is asked for long after launch and can only answer with a command or none,
    // so a take-over command that doesn't fill refuses the launch instead.
    take_over_args(manifest, "", &req.workdir)?;

    let role_args = match req.role {
        SessionRole::ReadOnly => &manifest.read_only_args,
        // A manifest names no direct flags, so a direct turn builds as a run's session does.
        SessionRole::Build | SessionRole::Direct => &manifest.build_args,
    };
    let uses = |name: &str| {
        role_args
            .iter()
            .chain(&manifest.resume_args)
            .any(|arg| placeholders_in(arg).contains(&name))
    };
    let scratch = req.scratch_dir.as_deref();
    let in_scratch = |file: &str| -> anyhow::Result<PathBuf> {
        scratch.map(|dir| dir.join(file)).ok_or_else(|| {
            anyhow!("{id} needs a session folder for {file}, and the request has none")
        })
    };

    // A CLI that takes no schema itself learns the report's shape from the prompt.
    let wants_schema = manifest.schema.is_some() || uses("schema_file");
    let prompt = if wants_schema {
        req.prompt.clone()
    } else {
        req.prompt_with_schema()
    };

    let mut files = Vec::new();
    let prompt_file = if manifest.prompt == PromptChannel::File || uses("prompt_file") {
        let path = in_scratch(session_files::PROMPT)?;
        files.push(SpecFile {
            path: path.clone(),
            contents: prompt.clone(),
        });
        Some(path)
    } else {
        None
    };
    let schema_file = match &req.json_schema {
        Some(schema) if wants_schema => {
            let path = in_scratch(session_files::SCHEMA)?;
            files.push(SpecFile {
                path: path.clone(),
                contents: serde_json::to_string_pretty(schema)?,
            });
            Some(path)
        }
        _ => None,
    };
    let home = match &manifest.home {
        Some(template) => {
            let home = in_scratch(session_files::HOME)?;
            let root = match templates_root {
                Some(root) => root.to_path_buf(),
                None => argonauts_home()?.join(user_files::CLIS),
            };
            files.extend(home_files(template, &root, &home)?);
            Some(home)
        }
        None if uses("home") => Some(in_scratch(session_files::HOME)?),
        None => None,
    };

    let values = Values {
        manifest,
        req,
        prompt: &prompt,
        workdir: text(&req.workdir),
        prompt_file: prompt_file.as_deref().map(text),
        schema_file: schema_file.as_deref().map(text),
        home: home.as_deref().map(text),
    };

    let mut args = fill_args(role_args, |name| values.fill(name, None))
        .map_err(|err| anyhow!("{id} arguments: {err}"))?;
    if let (Some(flag), Some(path)) = (&manifest.schema, &schema_file) {
        args.push(flag.flag.clone());
        args.push(text(path));
    }
    if let Some(session) = req.resume_session.as_deref() {
        if manifest.resume_args.is_empty() {
            bail!("{id} has no resume_args, so it can't resume session {session}");
        }
        let resume = fill_args(&manifest.resume_args, |name| {
            values.fill(name, Some(session))
        })
        .map_err(|err| anyhow!("{id} resume_args: {err}"))?;
        args.extend(resume);
    }
    args.extend(req.extra_args.iter().cloned());

    let mut env_set: Vec<(String, String)> = manifest
        .env_set
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    if let (Some(template), Some(home)) = (&manifest.home, &home) {
        env_set.push((template.env.clone(), text(home)));
    }
    env_set.extend(req.env_set.iter().cloned());
    let keep: Vec<&str> = manifest.env.iter().map(String::as_str).collect();

    Ok(CommandSpec {
        program: manifest.binary.clone(),
        args,
        env_set,
        env_remove: env_remove_except(&keep),
        stdin: (manifest.prompt == PromptChannel::Stdin).then(|| prompt.clone()),
        cwd: req.workdir.clone(),
        files,
        keep_stdin_open: false,
    })
}

/// What the placeholders become in one launch.
struct Values<'a> {
    manifest: &'a CliManifest,
    req: &'a SessionRequest,
    /// The prompt as sent, with the report's schema when the CLI takes none itself.
    prompt: &'a str,
    workdir: String,
    prompt_file: Option<String>,
    schema_file: Option<String>,
    home: Option<String>,
}

impl Values<'_> {
    /// `session` is set only while filling `resume_args`.
    fn fill(&self, name: &str, session: Option<&str>) -> Fill {
        match name {
            "model" => optional(self.req.model.as_deref()),
            "effort" => optional(self.req.effort.as_deref()),
            "workdir" => Fill::Value(self.workdir.clone()),
            "schema_file" => optional(self.schema_file.as_deref()),
            "prompt_file" => required(self.prompt_file.as_deref(), "no prompt file was written"),
            "home" => required(self.home.as_deref(), "there's no session folder"),
            "session" => required(session, "only resume_args and interactive_resume get one"),
            "prompt" if self.manifest.prompt == PromptChannel::Argument => {
                Fill::Value(self.prompt.to_owned())
            }
            _ => Fill::Refuse("allowed only with prompt = \"argument\"".into()),
        }
    }
}

/// The take-over command: `interactive_resume` with `{session}` and `{workdir}`. `None` for a
/// manifest with a home template, since only the session folder knows that home and the
/// take-over would otherwise run on your real one, without the session.
pub(crate) fn interactive_resume(
    manifest: &CliManifest,
    session_id: &str,
    workdir: &Path,
) -> anyhow::Result<Option<CommandSpec>> {
    if manifest.interactive_resume.is_empty() || manifest.home.is_some() {
        return Ok(None);
    }
    let args = take_over_args(manifest, session_id, workdir)?;
    let mut spec = CommandSpec::new(manifest.binary.clone(), workdir).args(args);
    spec.env_set = manifest
        .env_set
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    Ok(Some(spec))
}

fn take_over_args(
    manifest: &CliManifest,
    session_id: &str,
    workdir: &Path,
) -> anyhow::Result<Vec<String>> {
    let workdir = text(workdir);
    fill_args(&manifest.interactive_resume, |name| match name {
        "session" => Fill::Value(session_id.to_owned()),
        "workdir" => Fill::Value(workdir.clone()),
        _ => Fill::Refuse("the take-over command gets only {session} and {workdir}".into()),
    })
    .map_err(|err| anyhow!("{} interactive_resume: {err}", manifest.id))
}

fn optional(value: Option<&str>) -> Fill {
    value.map_or(Fill::Omit, |value| Fill::Value(value.to_owned()))
}

fn required(value: Option<&str>, why_not: &str) -> Fill {
    value.map_or_else(
        || Fill::Refuse(why_not.to_owned()),
        |value| Fill::Value(value.to_owned()),
    )
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::models::registry::cli::{HomeKeep, HomeTemplate};

    fn with_take_over(args: &[&str]) -> CliManifest {
        CliManifest {
            id: "cli".into(),
            binary: "cli".into(),
            interactive_resume: args.iter().map(|arg| (*arg).to_owned()).collect(),
            ..CliManifest::default()
        }
    }

    fn request() -> SessionRequest {
        SessionRequest {
            workdir: "/work".into(),
            prompt: "hi".into(),
            ..SessionRequest::default()
        }
    }

    #[test]
    fn a_home_template_means_no_take_over() {
        let manifest = CliManifest {
            home: Some(HomeTemplate {
                env: "CLI_HOME".into(),
                template: "cli".into(),
                keep: HomeKeep::Task,
            }),
            ..with_take_over(&["--resume", "{session}"])
        };
        let take_over = interactive_resume(&manifest, "s-1", Path::new("/work")).unwrap();
        assert!(take_over.is_none());
    }

    #[test]
    fn a_take_over_that_doesnt_fill_is_an_error() {
        let manifest = with_take_over(&["--resume", "{session}", "--model", "{model}"]);
        let err = interactive_resume(&manifest, "s-1", Path::new("/work")).unwrap_err();
        assert!(err.to_string().contains("{session} and {workdir}"), "{err}");
        let err = build(&manifest, &request(), None).unwrap_err();
        assert!(err.to_string().contains("interactive_resume"), "{err}");

        let fine = with_take_over(&["--resume", "{session}", "--cwd", "{workdir}"]);
        assert!(build(&fine, &request(), None).is_ok());
    }
}
