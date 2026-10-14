//! `/connect <hermes|openclaw>`: lets an outside agent drive Argonauts Code over MCP. It shows the
//! entry the outside agent's config needs, then writes it only after you confirm, copying the file
//! aside first. The outside agent can start, watch, pause, and guide runs; approvals stay here.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::input::OutsideAgent;
use argonauts_core::names::ChoiceId;

use super::{Ctx, Effect};
use crate::choices::{self, NOT_CHANGED};

type Handled = anyhow::Result<Vec<Effect>>;

/// The MCP server's name in the outside agent's config.
const SERVER: &str = "argonauts";

#[derive(Default)]
pub(crate) struct ConnectState {
    /// Open `ConfirmConnect` questions.
    pending: HashMap<ChoiceId, Pending>,
}

struct Pending {
    outside_agent: OutsideAgent,
    file: PathBuf,
    server: Server,
}

/// How the outside agent starts Argonauts Code's MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Server {
    pub argonauts: PathBuf,
    pub repo: PathBuf,
}

/// What the outside agent's config file needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Edit {
    /// The file with the entry added.
    Write(String),
    /// It names an `argonauts` server already.
    Present,
    /// The file is in a shape this can't edit safely, so you paste the entry yourself.
    ByHand(String),
}

pub(crate) fn owns_choice(state: &ConnectState, id: &ChoiceId) -> bool {
    state.pending.contains_key(id)
}

pub(crate) fn connect(
    ctx: &mut Ctx,
    state: &mut ConnectState,
    outside_agent: OutsideAgent,
) -> Handled {
    let Some(home) = ctx.env.user_home.clone() else {
        anyhow::bail!("can't find your home directory, so /connect can't find the config file");
    };
    let file = config_file(outside_agent, &home);
    let server = Server {
        argonauts: std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .context("finding the argonauts program")?,
        repo: ctx.env.repo_root.clone(),
    };
    let shown = ctx.env.display(&file);
    let existing = read(&file)?;
    let edit = match outside_agent {
        OutsideAgent::Hermes => hermes_edit(existing.as_deref(), &server),
        OutsideAgent::OpenClaw => openclaw_edit(existing.as_deref(), &server),
    };
    let name = display_name(outside_agent);
    let mut lines = vec![
        format!(
            "{name} starts `argonauts mcp serve` for this repo and can start, watch, pause, and \
             guide runs. Approvals stay here, and only while this repo's settings are trusted."
        ),
        String::new(),
        format!("In {shown}:"),
    ];
    lines.extend(
        entry(outside_agent, &server)
            .lines()
            .map(|line| format!("  {line}")),
    );
    if outside_agent == OutsideAgent::OpenClaw {
        lines.push(String::new());
        lines.push(format!("Or run: {}", openclaw_command(&server)));
    }
    let title = format!("Connect {name}");
    match edit {
        Edit::Present => {
            lines.push(String::new());
            lines.push(format!(
                "{shown} names an `{SERVER}` server already, so nothing changes. Edit it there."
            ));
            ctx.send(AppEvent::Report { title, lines });
        }
        Edit::ByHand(why) => {
            lines.push(String::new());
            lines.push(format!("{why} Paste the entry in yourself."));
            ctx.send(AppEvent::Report { title, lines });
        }
        Edit::Write(_) => {
            ctx.send(AppEvent::Report { title, lines });
            let id = ctx.next_choice_id();
            let body = vec![if existing.is_some() {
                format!("{shown} is copied to {} first.", backup_display(ctx, &file))
            } else {
                format!("{shown} doesn't exist yet, so it's created.")
            }];
            ctx.send(AppEvent::Choice {
                choice: choices::app_choice(
                    id.clone(),
                    ChoiceKind::ConfirmConnect,
                    format!("Add the `{SERVER}` server to {shown}?"),
                    body,
                    choices::options(ChoiceKind::ConfirmConnect),
                ),
            });
            state.pending.insert(
                id,
                Pending {
                    outside_agent,
                    file,
                    server,
                },
            );
        }
    }
    Ok(Vec::new())
}

pub(crate) fn answer_choice(
    ctx: &mut Ctx,
    state: &mut ConnectState,
    id: ChoiceId,
    action: AnswerAction,
) -> Handled {
    let Some(pending) = state.pending.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    match action {
        AnswerAction::Approve => {
            let outcome = match write(&pending) {
                Ok(outcome) => outcome,
                Err(err) => {
                    close(ctx, id, NOT_CHANGED);
                    return Err(err);
                }
            };
            let shown = ctx.env.display(&pending.file);
            let text = match outcome {
                Written::Created => format!("Created {shown}."),
                Written::Updated { backup } => {
                    format!(
                        "Updated {shown}. The old one is {}.",
                        ctx.env.display(&backup)
                    )
                }
                Written::Present => format!("{shown} names an `{SERVER}` server already."),
            };
            close(ctx, id, text);
            ctx.notice(
                NoteLevel::Info,
                format!(
                    "Restart {} so it starts the server.",
                    display_name(pending.outside_agent)
                ),
            );
        }
        AnswerAction::Cancel => close(ctx, id, NOT_CHANGED),
        other => {
            state.pending.insert(id, pending);
            anyhow::bail!("{other:?} doesn't answer whether to write the config");
        }
    }
    Ok(Vec::new())
}

/// Esc: not now, and nothing changes.
pub(crate) fn set_aside(ctx: &mut Ctx, state: &mut ConnectState, id: ChoiceId) -> Handled {
    if state.pending.remove(&id).is_some() {
        close(ctx, id, NOT_CHANGED);
    }
    Ok(Vec::new())
}

fn close(ctx: &Ctx, id: ChoiceId, outcome: impl Into<String>) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.into(),
    });
}

fn display_name(outside_agent: OutsideAgent) -> &'static str {
    match outside_agent {
        OutsideAgent::Hermes => "Hermes",
        OutsideAgent::OpenClaw => "OpenClaw",
    }
}

/// `~/.hermes/config.yaml`, or `$HERMES_HOME/config.yaml` for a Hermes home elsewhere, and
/// `~/.openclaw/openclaw.json`.
fn config_file(outside_agent: OutsideAgent, home: &Path) -> PathBuf {
    match outside_agent {
        OutsideAgent::Hermes => std::env::var_os("HERMES_HOME")
            .filter(|dir| !dir.is_empty())
            .map_or_else(|| home.join(".hermes"), PathBuf::from)
            .join("config.yaml"),
        OutsideAgent::OpenClaw => home.join(".openclaw").join("openclaw.json"),
    }
}

fn read(file: &Path) -> anyhow::Result<Option<String>> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", file.display())),
    }
}

/// A JSON string is also a YAML double-quoted scalar, so paths with spaces or quotes stay
/// whole.
fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.display().to_string()).unwrap_or_default()
}

/// The entry as the config holds it.
pub(super) fn entry(outside_agent: OutsideAgent, server: &Server) -> String {
    match outside_agent {
        OutsideAgent::Hermes => hermes_entry(server, ""),
        OutsideAgent::OpenClaw => {
            let value =
                serde_json::json!({ "mcp": { "servers": { SERVER: openclaw_server(server) } } });
            serde_json::to_string_pretty(&value).unwrap_or_default()
        }
    }
}

/// `mcp_servers:` and the entry under it, each line after `indent`.
fn hermes_entry(server: &Server, indent: &str) -> String {
    format!(
        "{indent}mcp_servers:\n{indent}  {SERVER}:\n{indent}    command: {}\n{indent}    args: [\"mcp\", \"serve\", \"--repo\", {}]\n",
        quoted(&server.argonauts),
        quoted(&server.repo)
    )
}

/// Adds the entry under `mcp_servers:`, or adds that key at the end. Only the block form is
/// edited; anything else is left for you.
pub(super) fn hermes_edit(existing: Option<&str>, server: &Server) -> Edit {
    let Some(text) = existing.filter(|text| !text.trim().is_empty()) else {
        return Edit::Write(hermes_entry(server, ""));
    };
    let lines: Vec<&str> = text.lines().collect();
    let key = lines.iter().position(|line| {
        line.starts_with("mcp_servers:")
            && line["mcp_servers:".len()..]
                .trim_start()
                .split('#')
                .next()
                .is_some_and(|rest| rest.trim().is_empty())
    });
    if key.is_none() && lines.iter().any(|line| line.starts_with("mcp_servers")) {
        return Edit::ByHand(
            "Its `mcp_servers` isn't a plain block, so it isn't changed.".to_owned(),
        );
    }
    let Some(key) = key else {
        let mut out = text.to_owned();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&hermes_entry(server, ""));
        return Edit::Write(out);
    };
    // The block runs until the next line that isn't indented, a comment, or blank.
    let block: Vec<&str> = lines[key + 1..]
        .iter()
        .take_while(|line| line.trim().is_empty() || line.starts_with([' ', '\t', '#']))
        .copied()
        .collect();
    let indent = block
        .iter()
        .find(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map_or("  ", |line| &line[..line.len() - line.trim_start().len()]);
    let named = block.iter().any(|line| {
        line.strip_prefix(indent)
            .is_some_and(|rest| rest.starts_with(&format!("{SERVER}:")))
    });
    if named {
        return Edit::Present;
    }
    let entry = format!(
        "{indent}{SERVER}:\n{indent}{indent}command: {}\n{indent}{indent}args: [\"mcp\", \"serve\", \"--repo\", {}]",
        quoted(&server.argonauts),
        quoted(&server.repo)
    );
    let mut out: Vec<String> = lines[..=key]
        .iter()
        .map(|line| (*line).to_owned())
        .collect();
    out.push(entry);
    out.extend(lines[key + 1..].iter().map(|line| (*line).to_owned()));
    let mut out = out.join("\n");
    out.push('\n');
    Edit::Write(out)
}

fn openclaw_server(server: &Server) -> serde_json::Value {
    serde_json::json!({
        "command": server.argonauts.display().to_string(),
        "args": ["mcp", "serve"],
        "cwd": server.repo.display().to_string(),
    })
}

/// The command OpenClaw's own docs give for adding a server.
fn openclaw_command(server: &Server) -> String {
    let quote = |path: &Path| shell_quote(&path.display().to_string());
    format!(
        "openclaw mcp add {SERVER} --command {} --arg mcp --arg serve --cwd {}",
        quote(&server.argonauts),
        quote(&server.repo)
    )
}

/// Quotes `word` for a POSIX shell when it needs it.
fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@".contains(c));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// Adds `mcp.servers.argonauts` to the JSON. A file with comments or other JSON5 is left for
/// `openclaw mcp add`.
pub(super) fn openclaw_edit(existing: Option<&str>, server: &Server) -> Edit {
    let mut config = match existing.filter(|text| !text.trim().is_empty()) {
        None => serde_json::json!({}),
        Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(config) => config,
            Err(_) => {
                return Edit::ByHand(
                    "It isn't plain JSON, so it isn't changed. `openclaw mcp add` can add it."
                        .to_owned(),
                );
            }
        },
    };
    let Some(root) = config.as_object_mut() else {
        return Edit::ByHand("It isn't a JSON object, so it isn't changed.".to_owned());
    };
    let mcp = root.entry("mcp").or_insert_with(|| serde_json::json!({}));
    let Some(servers) = mcp
        .as_object_mut()
        .map(|mcp| {
            mcp.entry("servers")
                .or_insert_with(|| serde_json::json!({}))
        })
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Edit::ByHand("Its `mcp.servers` isn't an object, so it isn't changed.".to_owned());
    };
    if servers.contains_key(SERVER) {
        return Edit::Present;
    }
    servers.insert(SERVER.to_owned(), openclaw_server(server));
    let mut text = serde_json::to_string_pretty(&config).unwrap_or_default();
    text.push('\n');
    Edit::Write(text)
}

enum Written {
    Created,
    Updated { backup: PathBuf },
    Present,
}

/// Plans again from the file as it is now, so a change made meanwhile is kept, then copies the
/// old file aside and writes the new one in its place.
fn write(pending: &Pending) -> anyhow::Result<Written> {
    let file = &pending.file;
    let existing = read(file)?;
    let edit = match pending.outside_agent {
        OutsideAgent::Hermes => hermes_edit(existing.as_deref(), &pending.server),
        OutsideAgent::OpenClaw => openclaw_edit(existing.as_deref(), &pending.server),
    };
    let text = match edit {
        Edit::Write(text) => text,
        Edit::Present => return Ok(Written::Present),
        Edit::ByHand(why) => anyhow::bail!("{} changed: {why}", file.display()),
    };
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let backup = match existing {
        Some(_) => {
            let backup = backup_path(file);
            // Keeps the file's permissions, which matter when it holds a token.
            fs::copy(file, &backup).with_context(|| format!("copying {} aside", file.display()))?;
            Some(backup)
        }
        None => None,
    };
    let partial = file.with_extension("argonauts-partial");
    fs::write(&partial, text).with_context(|| format!("writing {}", partial.display()))?;
    if let Ok(meta) = fs::metadata(file) {
        fs::set_permissions(&partial, meta.permissions())
            .with_context(|| format!("writing {}", partial.display()))?;
    }
    fs::rename(&partial, file).with_context(|| format!("writing {}", file.display()))?;
    Ok(match backup {
        Some(backup) => Written::Updated { backup },
        None => Written::Created,
    })
}

/// `config.yaml.argonauts-backup`, or `.argonauts-backup-2` and on when that's taken, so an
/// earlier backup is never overwritten.
fn backup_path(file: &Path) -> PathBuf {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut n = 1;
    loop {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        let path = file.with_file_name(format!("{name}.argonauts-backup{suffix}"));
        if !path.exists() {
            return path;
        }
        n += 1;
    }
}

fn backup_display(ctx: &Ctx, file: &Path) -> String {
    ctx.env.display(&backup_path(file))
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::{AnswerTarget, AppEvent, UiCommand};
    use argonauts_core::events::approval::AnswerAction;
    use argonauts_core::input::{OutsideAgent, SlashCommand};

    use super::*;
    use crate::controller::test_support::TestDriver;

    fn server() -> Server {
        Server {
            argonauts: PathBuf::from("/opt/bin/argonauts"),
            repo: PathBuf::from("/code/my repo"),
        }
    }

    #[test]
    fn hermes_gets_a_new_block_or_an_entry_under_its_own() {
        let Edit::Write(fresh) = hermes_edit(None, &server()) else {
            panic!()
        };
        assert_eq!(
            fresh,
            "mcp_servers:\n  argonauts:\n    command: \"/opt/bin/argonauts\"\n    args: [\"mcp\", \"serve\", \"--repo\", \"/code/my repo\"]\n"
        );

        let appended = hermes_edit(Some("model: x\n"), &server());
        assert_eq!(appended, Edit::Write(format!("model: x\n\n{fresh}")));

        let under = "model: x\nmcp_servers:  # yours\n    github:\n        command: gh\nother: 1\n";
        let Edit::Write(text) = hermes_edit(Some(under), &server()) else {
            panic!()
        };
        assert_eq!(
            text,
            "model: x\nmcp_servers:  # yours\n    argonauts:\n        command: \"/opt/bin/argonauts\"\n        args: [\"mcp\", \"serve\", \"--repo\", \"/code/my repo\"]\n    github:\n        command: gh\nother: 1\n"
        );
    }

    #[test]
    fn hermes_already_connected_or_odd_is_left_alone() {
        let present = "mcp_servers:\n  argonauts:\n    command: old\n";
        assert_eq!(hermes_edit(Some(present), &server()), Edit::Present);
        let inline = "mcp_servers: {}\n";
        assert!(matches!(
            hermes_edit(Some(inline), &server()),
            Edit::ByHand(_)
        ));
    }

    #[test]
    fn openclaw_gets_mcp_servers_argonauts_in_its_json() {
        let Edit::Write(text) = openclaw_edit(Some(r#"{"gateway": {"port": 18789}}"#), &server())
        else {
            panic!()
        };
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["gateway"]["port"], 18789);
        assert_eq!(
            value["mcp"]["servers"]["argonauts"],
            serde_json::json!({"command": "/opt/bin/argonauts", "args": ["mcp", "serve"], "cwd": "/code/my repo"})
        );
        assert_eq!(openclaw_edit(Some(&text), &server()), Edit::Present);
        assert!(matches!(
            openclaw_edit(Some("{ // json5\n}"), &server()),
            Edit::ByHand(_)
        ));
        assert!(openclaw_command(&server()).ends_with("--cwd '/code/my repo'"));
    }

    fn connect(outside_agent: OutsideAgent) -> UiCommand {
        UiCommand::Slash {
            command: SlashCommand::Connect { outside_agent },
        }
    }

    fn answer(id: ChoiceId, action: AnswerAction) -> UiCommand {
        UiCommand::Answer {
            to: AnswerTarget::Choice { id },
            action,
            text: None,
            evidence_sha256: None,
            tasks: Vec::new(),
            remember: false,
        }
    }

    async fn question(driver: &mut TestDriver) -> ChoiceId {
        let event = driver
            .until(|event| matches!(event, AppEvent::Choice { .. }))
            .await;
        match event {
            AppEvent::Choice { choice } => {
                assert_eq!(choice.kind, ChoiceKind::ConfirmConnect);
                choice.id
            }
            _ => unreachable!(),
        }
    }

    async fn closed(driver: &mut TestDriver) -> String {
        let event = driver
            .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
            .await;
        match event {
            AppEvent::ChoiceClosed { outcome, .. } => outcome,
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn hermes_config_is_written_only_after_yes_with_a_backup() {
        let mut driver = TestDriver::start().await;
        let file = driver.repo.user_home().join(".hermes").join("config.yaml");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "model: x\n").unwrap();

        driver.send(connect(OutsideAgent::Hermes)).await;
        let report = driver
            .until(|event| matches!(event, AppEvent::Report { .. }))
            .await;
        let AppEvent::Report { title, lines } = report else {
            unreachable!()
        };
        assert_eq!(title, "Connect Hermes");
        assert!(
            lines.iter().any(|line| line.contains("mcp_servers:")),
            "{lines:?}"
        );
        let id = question(&mut driver).await;
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "model: x\n",
            "nothing yet"
        );

        driver.send(answer(id, AnswerAction::Approve)).await;
        let outcome = closed(&mut driver).await;
        assert!(
            outcome.starts_with("Updated ~/.hermes/config.yaml"),
            "{outcome}"
        );
        let written = fs::read_to_string(&file).unwrap();
        assert!(
            written.starts_with("model: x\n\nmcp_servers:\n  argonauts:\n"),
            "{written}"
        );
        assert!(
            written.contains("\"mcp\", \"serve\", \"--repo\""),
            "{written}"
        );
        let backup = file.with_file_name("config.yaml.argonauts-backup");
        assert_eq!(fs::read_to_string(backup).unwrap(), "model: x\n");

        // Connected now, so a second /connect changes nothing and asks nothing.
        driver.send(connect(OutsideAgent::Hermes)).await;
        let AppEvent::Report { lines, .. } = driver
            .until(|event| matches!(event, AppEvent::Report { .. }))
            .await
        else {
            unreachable!()
        };
        assert!(lines.last().unwrap().contains("already"), "{lines:?}");
        driver.exit().await;
    }

    #[tokio::test]
    async fn no_leaves_the_openclaw_config_alone() {
        let mut driver = TestDriver::start().await;
        let file = driver
            .repo
            .user_home()
            .join(".openclaw")
            .join("openclaw.json");
        driver.send(connect(OutsideAgent::OpenClaw)).await;
        let id = question(&mut driver).await;
        driver.send(answer(id, AnswerAction::Cancel)).await;
        assert_eq!(closed(&mut driver).await, NOT_CHANGED);
        assert!(!file.exists());

        driver.send(connect(OutsideAgent::OpenClaw)).await;
        let id = question(&mut driver).await;
        driver.send(answer(id, AnswerAction::Approve)).await;
        assert_eq!(
            closed(&mut driver).await,
            "Created ~/.openclaw/openclaw.json."
        );
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(value["mcp"]["servers"]["argonauts"]["args"][1], "serve");
        driver.exit().await;
    }

    #[test]
    fn a_backup_never_overwrites_an_earlier_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.yaml");
        assert_eq!(
            backup_path(&file),
            dir.path().join("config.yaml.argonauts-backup")
        );
        fs::write(dir.path().join("config.yaml.argonauts-backup"), "").unwrap();
        assert_eq!(
            backup_path(&file),
            dir.path().join("config.yaml.argonauts-backup-2")
        );
    }
}
