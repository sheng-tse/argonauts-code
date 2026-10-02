//! Recording exactly what was sent. The first event of a session is `Session(Input)` with the
//! launch (arguments with the prompt replaced by `<prompt>`, environment names only, how the
//! prompt went in, and the sandbox in one line) and the prompt parts. The prompt sent is the
//! parts' text, concatenated byte for byte.

use argonauts_core::CommandSpec;
use argonauts_core::events::session::{
    InputPart, InputPartKind, LaunchRecord, PromptVia, QueuedMessage, SessionEvent, SessionInput,
};

pub const PROMPT_PLACEHOLDER: &str = "<prompt>";

/// What goes to the harness, byte for byte.
pub fn prompt_text(parts: &[InputPart]) -> String {
    parts.iter().map(|part| part.text.as_str()).collect()
}

pub fn launch_record(command: &CommandSpec, prompt: &str, sandbox: &str) -> LaunchRecord {
    LaunchRecord {
        program: command.program.clone(),
        args: command
            .args
            .iter()
            .map(|arg| without_prompt(arg, prompt))
            .collect(),
        cwd: command.cwd.clone(),
        env_set: command
            .env_set
            .iter()
            .map(|(name, _)| name.clone())
            .collect(),
        env_removed: command.env_remove.clone(),
        prompt_via: prompt_via(command, prompt),
        sandbox: Some(sandbox.to_owned()),
    }
}

fn without_prompt(arg: &str, prompt: &str) -> String {
    if prompt.is_empty() {
        arg.to_owned()
    } else {
        arg.replace(prompt, PROMPT_PLACEHOLDER)
    }
}

/// An argument that holds the prompt wins, then an open input stream, then stdin.
pub fn prompt_via(command: &CommandSpec, prompt: &str) -> PromptVia {
    let in_args = !prompt.is_empty() && command.args.iter().any(|arg| arg.contains(prompt));
    if in_args {
        PromptVia::Argument
    } else if command.keep_stdin_open {
        PromptVia::StreamInput
    } else if command.stdin.is_some() {
        PromptVia::Stdin
    } else {
        PromptVia::Argument
    }
}

/// `Session(Input)`: every part at the start, and only the new ones after each delivery.
pub fn input_event(
    launch: LaunchRecord,
    parts: Vec<InputPart>,
    queued: Vec<QueuedMessage>,
) -> SessionEvent {
    SessionEvent::Input(Box::new(SessionInput {
        launch,
        parts,
        queued,
    }))
}

/// A part the worker adds after the start: a delivered message, the wrap-up, or a note.
pub fn part(kind: InputPartKind, label: &str, text: &str, source: Option<String>) -> InputPart {
    InputPart {
        kind,
        label: label.to_owned(),
        text: text.to_owned(),
        approx_tokens: approx_tokens(text),
        source,
    }
}

/// About 4 characters a token.
fn approx_tokens(text: &str) -> u32 {
    u32::try_from(text.chars().count().div_ceil(4)).unwrap_or(u32::MAX)
}

pub fn message_part(message: &QueuedMessage) -> InputPart {
    part(
        InputPartKind::Message,
        "message",
        &message.text,
        Some(message.id.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::session::InputPartKind;

    #[test]
    fn the_prompt_and_env_values_stay_out_of_the_record() {
        let mut command = CommandSpec::new("agent", "/w").args([
            "-p",
            "fix the bug",
            "--flag=fix the bug",
            "--model",
            "m",
        ]);
        command.env_set = vec![("SECRET_TOKEN".into(), "hunter2".into())];
        command.env_remove = vec!["CLAUDECODE".into()];
        let record = launch_record(&command, "fix the bug", "seatbelt");
        assert_eq!(
            record.args,
            ["-p", "<prompt>", "--flag=<prompt>", "--model", "m"]
        );
        assert_eq!(record.env_set, ["SECRET_TOKEN"]);
        assert_eq!(record.env_removed, ["CLAUDECODE"]);
        assert_eq!(record.prompt_via, PromptVia::Argument);
        assert_eq!(record.sandbox.as_deref(), Some("seatbelt"));
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("hunter2") && !json.contains("fix the bug"));
    }

    #[test]
    fn prompt_via_follows_the_command() {
        let mut command = CommandSpec::new("agent", "/w");
        command.stdin = Some("hi".into());
        assert_eq!(prompt_via(&command, "hi"), PromptVia::Stdin);
        command.keep_stdin_open = true;
        assert_eq!(prompt_via(&command, "hi"), PromptVia::StreamInput);
        let command = command.arg("hi");
        assert_eq!(prompt_via(&command, "hi"), PromptVia::Argument);
    }

    #[test]
    fn prompt_is_the_parts_byte_for_byte() {
        let parts = vec![
            part(InputPartKind::Prompt, "role", "You fix bugs.\n", None),
            part(InputPartKind::Card, "card", "Card: é ✓\r\n", None),
            part(InputPartKind::Rules, "rules", "", None),
            part(
                InputPartKind::Goal,
                "goal",
                "Goal 🦀",
                Some("goal.md".into()),
            ),
        ];
        assert_eq!(prompt_text(&parts), "You fix bugs.\nCard: é ✓\r\nGoal 🦀");
        assert_eq!(parts[0].approx_tokens, 4);
    }
}
