//! Notifications when something needs you and the terminal isn't focused: the OS's own, and
//! the message command from your global settings. Their text is built by code from fixed
//! parts, never from agent text, and goes to the program as one argument, never through a shell.

use std::process::Stdio;
use std::time::Duration;

use argonauts_core::app::AppEvent;
use argonauts_core::names::ApprovalId;
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::settings::ui::ConversationTrigger;
use argonauts_core::{NoteLevel, RunId, Spend};
use tokio::process::Command;

use super::{Ctx, Outbox};
use crate::app_settings::AppSettings;

const CONVERSATION_COMMAND_KEY: &str = "notifications.conversation.command";
/// Only this layer may set the message command, so a cloned repo can't pick the program.
const GLOBAL_LAYER: &str = "global";
const TEXT_SLOT: &str = "{text}";
/// Messaging apps cut long messages anyway, and ours never need more.
const CONVERSATION_TEXT_MAX: usize = 500;
const OS_TEXT_MAX: usize = 200;
/// A notification program that hangs must not pile up processes.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    MacOs,
    Linux,
    Other,
}

impl Platform {
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// What a notification says. Every field is written by code.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NeedsYou {
    /// The repo folder's name.
    pub repo: String,
    pub run: RunId,
    pub approval: Option<ApprovalId>,
    /// Such as "plan approval", or for a finished run its outcome, such as "merged".
    pub kind: String,
    pub spent: Option<Spend>,
    pub trigger: ConversationTrigger,
}

/// The argument vector, program first: `osascript … -- <body> <title>` on macOS,
/// `notify-send -- <title> <body>` on Linux. `None` elsewhere.
pub(crate) fn os_command(platform: Platform, title: &str, body: &str) -> Option<Vec<String>> {
    let title = one_line(title, OS_TEXT_MAX);
    let body = one_line(body, OS_TEXT_MAX);
    let argv: Vec<&str> = match platform {
        // The script reads the text from argv, so no quote in it can end the AppleScript string.
        Platform::MacOs => vec![
            "osascript",
            "-e",
            "on run argv",
            "-e",
            "display notification (item 1 of argv) with title (item 2 of argv)",
            "-e",
            "end run",
            "--",
            &body,
            &title,
        ],
        Platform::Linux => vec!["notify-send", "--", &title, &body],
        Platform::Other => return None,
    };
    Some(argv.into_iter().map(str::to_owned).collect())
}

/// The OS notification's title and body.
pub(crate) fn os_text(what: &NeedsYou) -> (String, String) {
    let title = format!("Argonauts Code · {}", what.repo);
    let body = match what.trigger {
        ConversationTrigger::NeedsYou => format!("{} needs you: {}", what.run, what.kind),
        ConversationTrigger::AgentDown => {
            format!("{}: {}, pick who continues", what.run, what.kind)
        }
        ConversationTrigger::Finished => format!("{} finished ({})", what.run, what.kind),
    };
    (title, body)
}

/// The message command's text: repo, run, approval, kind, spend, and where to answer. Refuses a
/// line starting with `MEDIA:` or holding `[[`, and caps the length.
pub(crate) fn conversation_text(what: &NeedsYou) -> anyhow::Result<String> {
    let repo = one_line(&what.repo, 80);
    let at = what
        .approval
        .as_ref()
        .map_or_else(|| "its approval".to_owned(), ToString::to_string);
    let spent = spent_text(what.spent.as_ref());
    let text = match what.trigger {
        ConversationTrigger::NeedsYou => format!(
            "Argonauts Code in {repo}: {run} waits for you at {at}, a {kind}. Spent so far: \
             {spent}. Answer in the argonauts app in {repo}.",
            run = what.run,
            kind = what.kind,
        ),
        ConversationTrigger::AgentDown => format!(
            "Argonauts Code in {repo}: {kind} in {run}, and {at} asks who continues. Spent so far: \
             {spent}. Answer in the argonauts app in {repo}.",
            run = what.run,
            kind = what.kind,
        ),
        ConversationTrigger::Finished => format!(
            "Argonauts Code in {repo}: {run} finished ({kind}). Spent: {spent}. Open the argonauts \
             app in {repo} to see it.",
            run = what.run,
            kind = what.kind,
        ),
    };
    refuse_attachments(&text)?;
    Ok(cap(&text, CONVERSATION_TEXT_MAX))
}

/// Hermes attaches the file a `MEDIA:<path>` token names, and `[[…]]` is another sender's
/// directive syntax, so text holding either never goes out.
fn refuse_attachments(text: &str) -> anyhow::Result<()> {
    let media = text
        .split_whitespace()
        .any(|word| word.to_ascii_uppercase().starts_with("MEDIA:"));
    if media {
        anyhow::bail!("the message's text holds a MEDIA: line, so it wasn't sent");
    }
    if text.contains("[[") {
        anyhow::bail!("the message's text holds [[, so it wasn't sent");
    }
    Ok(())
}

/// `notifications.conversation.command` with `{text}` replaced as one whole argument.
pub(crate) fn conversation_argv(template: &[String], text: &str) -> anyhow::Result<Vec<String>> {
    let Some(program) = template.first() else {
        anyhow::bail!("{CONVERSATION_COMMAND_KEY} is empty");
    };
    if !std::path::Path::new(program).is_absolute() {
        anyhow::bail!(
            "{CONVERSATION_COMMAND_KEY} must start with an absolute program path, not {program}"
        );
    }
    if template
        .iter()
        .any(|arg| arg.contains(TEXT_SLOT) && arg != TEXT_SLOT)
    {
        anyhow::bail!("{CONVERSATION_COMMAND_KEY} must hold {TEXT_SLOT} as one whole argument");
    }
    if !template.iter().any(|arg| arg == TEXT_SLOT) {
        anyhow::bail!("{CONVERSATION_COMMAND_KEY} has no {TEXT_SLOT} argument for the message");
    }
    Ok(template
        .iter()
        .map(|arg| {
            if arg == TEXT_SLOT {
                text.to_owned()
            } else {
                arg.clone()
            }
        })
        .collect())
}

/// The message command, when your global settings file sets one. Any other layer is ignored.
pub(crate) fn conversation_command(settings: &AppSettings) -> Option<&[String]> {
    let command = &settings.settings().notifications.conversation.command;
    if command.is_empty() {
        return None;
    }
    let layer = settings.entry(CONVERSATION_COMMAND_KEY)?.layer.as_str();
    (layer == GLOBAL_LAYER).then_some(command.as_slice())
}

/// Sends what the settings ask for, in the background. A failed send is only a notice.
pub(crate) fn send(ctx: &Ctx, what: &NeedsYou) {
    let notifications = &ctx.settings().notifications;
    let approval = matches!(
        what.trigger,
        ConversationTrigger::NeedsYou | ConversationTrigger::AgentDown
    );
    if notifications.os && approval {
        let (title, body) = os_text(what);
        if let Some(argv) = os_command(Platform::current(), &title, &body) {
            spawn(ctx.out.clone(), "OS", argv);
        }
    }
    if !notifications.conversation.on.contains(&what.trigger) {
        return;
    }
    let Some(template) = conversation_command(&ctx.settings) else {
        return;
    };
    match conversation_text(what).and_then(|text| conversation_argv(template, &text)) {
        Ok(argv) => spawn(ctx.out.clone(), "message", argv),
        Err(err) => ctx.notice(NoteLevel::Warn, format!("{err:#}")),
    }
}

/// One line about a run that isn't this conversation's, as an OS notification: it started in
/// another window, or it ended there. What needs you goes through [`send`].
pub(crate) fn send_os_line(ctx: &Ctx, repo: &str, line: &str) {
    if !ctx.settings().notifications.os {
        return;
    }
    let (title, body) = os_line(repo, line);
    if let Some(argv) = os_command(Platform::current(), &title, &body) {
        spawn(ctx.out.clone(), "OS", argv);
    }
}

/// The title and body of [`send_os_line`]'s notification.
fn os_line(repo: &str, line: &str) -> (String, String) {
    (format!("Argonauts Code · {repo}"), line.to_owned())
}

fn spawn(out: Outbox, which: &'static str, argv: Vec<String>) {
    tokio::spawn(async move {
        if let Err(err) = run(&argv).await {
            out.send(AppEvent::Notice {
                level: NoteLevel::Warn,
                text: format!("The {which} notification failed: {err:#}"),
            });
        }
    });
}

async fn run(argv: &[String]) -> anyhow::Result<()> {
    let Some((program, args)) = argv.split_first() else {
        anyhow::bail!("no program to run");
    };
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(SEND_TIMEOUT, output)
        .await
        .map_err(|_| anyhow::anyhow!("{program} took longer than {SEND_TIMEOUT:?}"))?
        .map_err(|err| anyhow::anyhow!("running {program}: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let first = one_line(stderr.lines().next().unwrap_or(""), OS_TEXT_MAX);
    anyhow::bail!("{program} exited with {}: {first}", output.status)
}

fn spent_text(spent: Option<&Spend>) -> String {
    match spent {
        Some(spent) if spent.cost_usd > 0.0 => argonauts_tui::format::spend(spent),
        Some(spent) if spent.input_tokens + spent.output_tokens > 0 => {
            format!("{} tokens", spent.input_tokens + spent.output_tokens)
        }
        _ => "nothing yet".to_owned(),
    }
}

/// Escapes and control characters dropped, lines joined, and the length capped.
fn one_line(text: &str, max: usize) -> String {
    let clean = sanitize_for_terminal(text).replace(['\n', '\t'], " ");
    cap(clean.trim(), max)
}

fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use argonauts_core::Paths;
    use argonauts_testkit::TempRepo;

    use super::*;
    use crate::env::AppEnv;

    fn needs_you(repo: &str) -> NeedsYou {
        NeedsYou {
            repo: repo.to_owned(),
            run: RunId::new("run-007").expect("a run id"),
            approval: ApprovalId::parse("approval-003"),
            kind: "plan approval".to_owned(),
            spent: Some(Spend {
                cost_usd: 1.2,
                ..Spend::default()
            }),
            trigger: ConversationTrigger::NeedsYou,
        }
    }

    #[test]
    fn os_notification_command() {
        let mac = os_command(Platform::MacOs, "Argonauts Code · api", "run-007 needs you");
        assert_eq!(
            mac,
            Some(
                [
                    "osascript",
                    "-e",
                    "on run argv",
                    "-e",
                    "display notification (item 1 of argv) with title (item 2 of argv)",
                    "-e",
                    "end run",
                    "--",
                    "run-007 needs you",
                    "Argonauts Code · api",
                ]
                .map(str::to_owned)
                .to_vec()
            )
        );
        let linux = os_command(Platform::Linux, "Argonauts Code · api", "run-007 needs you");
        assert_eq!(
            linux,
            Some(
                [
                    "notify-send",
                    "--",
                    "Argonauts Code · api",
                    "run-007 needs you"
                ]
                .map(str::to_owned)
                .to_vec()
            )
        );
        assert_eq!(os_command(Platform::Other, "t", "b"), None);
    }

    #[test]
    fn os_notification_text_is_argv_data() {
        let hostile =
            "\" & do shell script \"rm -rf ~\" & \"\n$(touch /tmp/x) `id`\u{1b}]0;title\u{7}";
        let argv = os_command(Platform::MacOs, "Argonauts Code", hostile).expect("a macOS command");
        assert_eq!(argv.len(), 10, "{argv:?}");
        let body = &argv[8];
        assert_eq!(argv[7], "--");
        assert!(body.contains("do shell script \"rm -rf ~\""), "{body}");
        assert!(body.contains("$(touch /tmp/x) `id`"), "{body}");
        assert!(!body.contains('\n') && !body.contains('\u{1b}'), "{body:?}");
        let linux = os_command(Platform::Linux, hostile, "body").expect("a Linux command");
        assert_eq!(linux[1], "--");
        assert!(linux[2].starts_with('"'), "{:?}", linux[2]);
        let long = "x".repeat(1000);
        let argv = os_command(Platform::Linux, "t", &long).expect("a Linux command");
        assert!(argv[3].chars().count() <= OS_TEXT_MAX);
    }

    #[test]
    fn conversation_notification_text_is_code_written() {
        let text = conversation_text(&needs_you("api")).expect("text");
        assert_eq!(
            text,
            "Argonauts Code in api: run-007 waits for you at approval-003, a plan approval. Spent \
             so far: $1.20. Answer in the argonauts app in api."
        );
        let finished = NeedsYou {
            approval: None,
            kind: "merged".to_owned(),
            trigger: ConversationTrigger::Finished,
            ..needs_you("api")
        };
        assert!(
            conversation_text(&finished)
                .expect("text")
                .contains("run-007 finished (merged)")
        );

        for repo in ["MEDIA:/etc/passwd", "x\nMEDIA:/tmp/key", "notes [[attach]]"] {
            assert!(conversation_text(&needs_you(repo)).is_err(), "{repo:?}");
        }
        let long = conversation_text(&needs_you(&"r".repeat(400))).expect("text");
        assert!(
            long.chars().count() <= CONVERSATION_TEXT_MAX,
            "{}",
            long.len()
        );

        let template: Vec<String> = ["/usr/local/bin/hermes", "send", "--quiet", "{text}"]
            .map(str::to_owned)
            .to_vec();
        let argv = conversation_argv(&template, "one; two && $(three)").expect("argv");
        assert_eq!(argv.len(), 4);
        assert_eq!(argv[3], "one; two && $(three)");
        let embedded = ["/bin/send", "--message={text}"]
            .map(str::to_owned)
            .to_vec();
        assert!(conversation_argv(&embedded, "hi").is_err());
        let relative = ["hermes", "{text}"].map(str::to_owned).to_vec();
        assert!(conversation_argv(&relative, "hi").is_err());
        let no_slot = ["/bin/send", "hello"].map(str::to_owned).to_vec();
        assert!(conversation_argv(&no_slot, "hi").is_err());
    }

    fn settings_with(global: Option<&str>, flags: Vec<String>) -> (TempRepo, AppSettings) {
        let repo = TempRepo::new().expect("a temp repo");
        let paths = Paths::new(repo.home());
        if let Some(text) = global {
            let path = paths.global_config();
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("the home dir");
            std::fs::write(&path, text).expect("the global config");
        }
        let env = AppEnv::for_repo(paths, repo.root(), true).expect("the app environment");
        let settings = AppSettings::load(&env, flags, true).expect("settings that resolve");
        (repo, settings)
    }

    #[test]
    fn conversation_notification_command_global_only() {
        let global =
            "[notifications.conversation]\ncommand = [\"/usr/local/bin/hermes\", \"{text}\"]\n";
        let (_repo, settings) = settings_with(Some(global), Vec::new());
        assert_eq!(
            conversation_command(&settings),
            Some(["/usr/local/bin/hermes".to_owned(), "{text}".to_owned()].as_slice())
        );

        let flag = r#"notifications.conversation.command=["/tmp/evil", "{text}"]"#.to_owned();
        let (_repo, settings) = settings_with(None, vec![flag]);
        assert_eq!(
            settings
                .settings()
                .notifications
                .conversation
                .command
                .first()
                .map(String::as_str),
            Some("/tmp/evil"),
            "the flag resolved, and only the app refuses it"
        );
        assert_eq!(conversation_command(&settings), None);

        let (_repo, settings) = settings_with(None, Vec::new());
        assert_eq!(conversation_command(&settings), None);
    }
}
