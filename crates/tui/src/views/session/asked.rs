//! The Input tab: what the agent was asked, part by part, each under a heading that says where
//! it came from. The launch command with its flags folds to one line at the end.
//!
//! The tab draws [`AskedPart`]s and nothing else. [`parts_of`] makes them from what a session
//! was sent: a part labelled `frame`, `orchestrator`, `your file` or `your request` is that
//! origin, and its source is who or what it came from. A part with any other label keeps the
//! label as its heading.

use argonauts_core::runs::events::session::{
    InputPart, InputPartKind, LaunchRecord, PromptVia, SessionInput,
};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::base::draw::panes;
use crate::base::text::{sanitize, sanitize_line, wrap_text};
use crate::base::theme::{Role, Theme};

/// Lines of a part shown before `→` expands it.
pub(super) const PART_LINES: usize = 6;

/// One part of what an agent reads, in the order it reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AskedPart {
    pub origin: Origin,
    /// Who or what within its origin: `built in`, the model that wrote it, or a file.
    pub from: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Origin {
    /// The role, what it may touch, and the shape of the answer.
    Frame,
    /// The orchestrator's instructions for this agent and this request.
    Orchestrator,
    /// Your own file for the role.
    YourFile,
    /// What your request said for the role.
    YourRequest,
    /// Anything else, under the label it was sent with, such as `notes`.
    Other(String),
    /// A part of the agent's head as the prompt's own assembly names it, such as
    /// `From the workflow · ship · Build` or `Instructions · built in`.
    Named(String),
}

impl AskedPart {
    /// `Frame · built in`, `From the orchestrator · Opus 5.5`, `Your file · roles/reviewer.md`.
    pub fn heading(&self) -> String {
        let origin = match &self.origin {
            Origin::Frame => "Frame".to_owned(),
            Origin::Orchestrator => "From the orchestrator".to_owned(),
            Origin::YourFile => "Your file".to_owned(),
            Origin::YourRequest => "From your request".to_owned(),
            Origin::Other(label) => capitalized(label),
            Origin::Named(heading) => return heading.clone(),
        };
        match self.from.as_deref().map(str::trim) {
            Some(from) if !from.is_empty() => format!("{origin} · {from}"),
            _ => origin,
        }
    }
}

fn capitalized(label: &str) -> String {
    let label = sanitize_line(label);
    let mut chars = label.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Part".to_owned(),
    }
}

/// The parts of one delivery, in the order they were sent, each without runs of blank lines.
/// The goal's summary repeats the request an earlier part already gave, so it shows once.
/// Every rule the agent was given reads under one `Rules` heading, where the last of them was
/// sent, so a rule never repeats its heading as its first words.
pub(super) fn parts_of(input: &SessionInput) -> Vec<AskedPart> {
    let mut parts: Vec<AskedPart> = Vec::new();
    let mut rules: Vec<String> = Vec::new();
    // A rule your own file or your request gave keeps its own heading.
    let is_rule = |part: &InputPart| {
        part.kind == InputPartKind::Rules && matches!(asked(part).origin, Origin::Other(_))
    };
    let last_rule = input.parts.iter().rposition(is_rule);
    for (at, part) in input.parts.iter().enumerate() {
        if is_rule(part) {
            let text = without_blank_runs(&part.text);
            if !text.is_empty() {
                rules.push(text);
            }
            if Some(at) == last_rule && !rules.is_empty() {
                parts.push(AskedPart {
                    origin: Origin::Named("Rules".to_owned()),
                    from: None,
                    text: std::mem::take(&mut rules).join("\n\n"),
                });
            }
            continue;
        }
        let mut asked = asked(part);
        if part.kind == InputPartKind::Goal {
            asked.text = without_said_summary(&asked.text, &parts);
        }
        asked.text = without_blank_runs(&asked.text);
        if !asked.text.is_empty() {
            parts.push(asked);
        }
    }
    parts
}

fn asked(part: &InputPart) -> AskedPart {
    // The frame, whoever wrote the instructions, your file and your request's words are named
    // by the assembly that put them in.
    if let Some(heading) = argonauts_core::prompts::prompt_parts::heading(part) {
        return AskedPart {
            origin: Origin::Named(sanitize_line(&heading).trim().to_owned()),
            from: None,
            text: part.text.clone(),
        };
    }
    let source = part
        .source
        .as_deref()
        .map(|source| sanitize_line(source).trim().to_owned())
        .filter(|source| !source.is_empty());
    let label = part.label.trim().to_ascii_lowercase();
    let (origin, from) = match label.as_str() {
        "frame" => (Origin::Frame, source.or_else(built_in)),
        "orchestrator" | "from the orchestrator" => (Origin::Orchestrator, source),
        "your file" | "role file" => (Origin::YourFile, source),
        "your request" | "from your request" => (Origin::YourRequest, source),
        // The role's own instructions are the frame, whatever today's templates label them.
        _ if part.kind == InputPartKind::Prompt => (Origin::Frame, source.or_else(built_in)),
        // The task under its id would read as code, so it reads as the task, in words.
        _ if part.kind == InputPartKind::Task => {
            return AskedPart {
                origin: Origin::Named("Task".to_owned()),
                from: None,
                text: task_text(&part.text),
            };
        }
        _ if part.kind == InputPartKind::Goal => (Origin::Other("Request".to_owned()), source),
        _ => (Origin::Other(part.label.clone()), source),
    };
    AskedPart {
        origin,
        from,
        text: part.text.clone(),
    }
}

fn built_in() -> Option<String> {
    Some("built in".to_owned())
}

/// A task as its title and kind in words, `add due dates · building`, then its other facts.
/// The engine writes it as `<id> · <title>`, a blank line, then `Kind: <kind>` and the rest.
fn task_text(text: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let title = first
        .split_once(" · ")
        .map_or(first, |(_, title)| title)
        .trim();
    let mut kind = None;
    let mut rest = Vec::new();
    for line in lines {
        match line.strip_prefix("Kind: ") {
            Some(words) if kind.is_none() => kind = Some(words.trim()),
            _ => rest.push(line),
        }
    }
    // A request run whole is its task's title too, cut short there: it reads once, in full.
    let said = |line: &&str| match title.strip_suffix('…') {
        Some(stem) => !stem.trim().is_empty() && line.trim().starts_with(stem.trim_end()),
        None => !title.is_empty() && line.trim() == title,
    };
    let title = match rest.iter().position(said) {
        Some(at) => {
            let words = rest.remove(at).trim();
            // A blank line before it goes too, when another follows or nothing does.
            let blank = |i: usize| rest.get(i).is_some_and(|line| line.trim().is_empty());
            if at > 0 && blank(at - 1) && (blank(at) || at == rest.len()) {
                rest.remove(at - 1);
            }
            words
        }
        None => title,
    };
    let mut head = title.to_owned();
    if let Some(kind) = kind.filter(|kind| !kind.is_empty()) {
        if !head.is_empty() {
            head.push_str(" · ");
        }
        head.push_str(kind);
    }
    std::iter::once(head.as_str())
        .chain(rest)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The goal file's text without its first line when an earlier part already says it.
fn without_said_summary(text: &str, earlier: &[AskedPart]) -> String {
    let Some(summary) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return String::new();
    };
    if !earlier.iter().any(|part| part.text.contains(summary)) {
        return text.to_owned();
    }
    let mut dropped = false;
    text.lines()
        .filter(|line| {
            let first = !dropped && line.trim() == summary;
            dropped |= first;
            !first
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One blank line at most between rows, and none at either end.
fn without_blank_runs(text: &str) -> String {
    let mut rows: Vec<&str> = Vec::new();
    for line in text.lines() {
        let blank = line.trim().is_empty();
        if blank && rows.last().is_none_or(|last| last.trim().is_empty()) {
            continue;
        }
        rows.push(if blank { "" } else { line });
    }
    while rows.last().is_some_and(|last| last.is_empty()) {
        rows.pop();
    }
    rows.join("\n")
}

/// What the tab's rows are built with.
pub(super) struct Draw<'a> {
    pub theme: &'a Theme,
    pub width: usize,
    /// The item the cursor is on, counting every part and then the launch line.
    pub selected: usize,
    /// The item `→` opened.
    pub expanded: Option<usize>,
}

/// The rows, and the row the selected item's heading is on.
pub(super) struct Drawn {
    pub lines: Vec<Line<'static>>,
    pub selected_row: usize,
}

/// How many items the cursor moves over: every part, and the launch line when there is one.
pub(super) fn items(inputs: &[SessionInput]) -> usize {
    let parts: usize = inputs.iter().map(|input| parts_of(input).len()).sum();
    parts + usize::from(!inputs.is_empty())
}

/// Each delivery's parts under their headings, then the launch folded to one line.
pub(super) fn lines(inputs: &[SessionInput], draw: &Draw<'_>) -> Drawn {
    let theme = draw.theme;
    let Some(first) = inputs.first() else {
        return Drawn {
            lines: vec![Line::from(Span::styled(
                "Nothing sent to this agent yet",
                theme.style(Role::Dim),
            ))],
            selected_row: 0,
        };
    };
    let mut lines = Vec::new();
    let mut selected_row = 0;
    let mut index = 0;
    for (n, input) in inputs.iter().enumerate() {
        if n > 0 {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!("Added by delivery {n}"),
                theme.style(Role::Bright),
            )));
        }
        for part in parts_of(input) {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            if index == draw.selected {
                selected_row = lines.len();
            }
            lines.extend(part_lines(&part, index, draw));
            index += 1;
        }
    }
    lines.push(Line::default());
    if index == draw.selected {
        selected_row = lines.len();
    }
    lines.extend(launch_lines(&first.launch, index, draw));
    Drawn {
        lines,
        selected_row,
    }
}

/// The heading in grey, and in the label color where the cursor is; then the text word for
/// word, each of its lines ending its own row.
fn part_lines(part: &AskedPart, index: usize, draw: &Draw<'_>) -> Vec<Line<'static>> {
    let theme = draw.theme;
    let dim = theme.style(Role::Dim);
    let mut lines = vec![heading(&part.heading(), index == draw.selected, theme)];
    let body = sanitize(&part.text);
    let rows: Vec<&str> = body.lines().collect();
    let shown = if draw.expanded == Some(index) {
        rows.len()
    } else {
        rows.len().min(PART_LINES)
    };
    let pad = [Span::raw("  ")];
    for row in &rows[..shown] {
        lines.extend(wrap_text(
            row,
            theme.style(Role::Text),
            draw.width,
            &pad,
            &pad,
        ));
    }
    if shown < rows.len() {
        let more = format!("  … +{} lines · →  expands", rows.len() - shown);
        lines.push(Line::from(Span::styled(
            panes::key_names(&more, theme),
            dim,
        )));
    }
    lines
}

fn heading(text: &str, selected: bool, theme: &Theme) -> Line<'static> {
    let style = if selected {
        theme.style(Role::Question).add_modifier(Modifier::BOLD)
    } else {
        theme.style(Role::Dim)
    };
    Line::from(Span::styled(text.to_owned(), style))
}

fn prompt_via(via: PromptVia) -> &'static str {
    match via {
        PromptVia::Argument => "prompt as an argument",
        PromptVia::Stdin => "prompt on stdin",
        PromptVia::StreamInput => "prompt on an open input stream",
        PromptVia::Request => "prompt in the request body",
    }
}

/// An argument as typed at a shell, quoted only when it has to be, so the line pastes back as
/// the same argv. Line breaks fold into spaces, the one change it makes.
fn shell_arg(arg: &str) -> String {
    shell_words::quote(&sanitize_line(arg)).into_owned()
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

/// `Launched as codex exec …`, cut to one row until `→` opens it: then the whole command, as
/// it pastes back at a shell, and where and how it ran.
fn launch_lines(launch: &LaunchRecord, index: usize, draw: &Draw<'_>) -> Vec<Line<'static>> {
    let theme = draw.theme;
    let dim = theme.style(Role::Dim);
    let command = std::iter::once(launch.program.as_str())
        .chain(launch.args.iter().map(String::as_str))
        .map(shell_arg)
        .collect::<Vec<_>>()
        .join(" ");
    let selected = index == draw.selected;
    if draw.expanded != Some(index) {
        let hint = panes::key_names(" · →  expands", theme);
        let opening = "Launched as ";
        let room = draw.width.saturating_sub(
            crate::base::text::str_width(opening) + crate::base::text::str_width(&hint),
        );
        let mut line = heading(opening, selected, theme);
        line.spans.push(Span::styled(
            crate::base::draw::layout::cut(&command, room),
            dim,
        ));
        line.spans.push(Span::styled(hint, dim));
        return vec![line];
    }
    let mut lines = vec![heading("Launched as", selected, theme)];
    lines.extend(wrap_text(
        &command,
        theme.style(Role::Text),
        draw.width,
        &[Span::styled("  $ ", dim)],
        &[Span::raw("      ")],
    ));
    let mut facts = vec![
        format!("cwd {}", clean(&launch.cwd.to_string_lossy())),
        prompt_via(launch.prompt_via).to_owned(),
    ];
    if let Some(sandbox) = &launch.sandbox {
        facts.push(format!("sandbox {}", clean(sandbox)));
    }
    if !launch.env_set.is_empty() {
        let names: Vec<String> = launch.env_set.iter().map(|name| clean(name)).collect();
        facts.push(format!("sets {}", names.join(", ")));
    }
    if !launch.env_removed.is_empty() {
        let names: Vec<String> = launch.env_removed.iter().map(|name| clean(name)).collect();
        facts.push(format!("clears {}", names.join(", ")));
    }
    lines.extend(wrap_text(
        &facts.join(" · "),
        dim,
        draw.width,
        &[Span::raw("  ")],
        &[Span::raw("  ")],
    ));
    lines
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::base::text::plain_text;
    use crate::base::theme::Glyphs;

    fn part(kind: InputPartKind, label: &str, source: Option<&str>, text: &str) -> InputPart {
        InputPart {
            kind,
            label: label.into(),
            text: text.into(),
            approx_tokens: 0,
            source: source.map(str::to_owned),
        }
    }

    fn launch() -> LaunchRecord {
        LaunchRecord {
            program: "codex".into(),
            args: vec![
                "exec".into(),
                "--json".into(),
                "--sandbox".into(),
                "workspace-write".into(),
                "<prompt>".into(),
            ],
            cwd: PathBuf::from("/wt/task-002"),
            env_set: vec!["ARGONAUTS_SESSION".into()],
            env_removed: Vec::new(),
            prompt_via: PromptVia::Stdin,
            sandbox: Some("worktree".into()),
        }
    }

    /// Every rule reads under one heading, and none repeats its heading as its first words.
    #[test]
    fn the_rules_read_under_one_heading() {
        let input = SessionInput {
            launch: launch(),
            parts: vec![
                part(InputPartKind::Prompt, "frame", None, "You are a worker."),
                part(
                    InputPartKind::Rules,
                    "decide, don't ask",
                    None,
                    "Decide, don't ask. Nobody reads a question at the end of your turn.",
                ),
                part(
                    InputPartKind::Task,
                    "task-001",
                    None,
                    "task-001 · add due dates",
                ),
                part(
                    InputPartKind::Rules,
                    "rules",
                    None,
                    "Never change .github/**.",
                ),
                part(InputPartKind::Status, "status", None, "Status of run-01"),
            ],
            queued: Vec::new(),
        };
        let headings: Vec<String> = parts_of(&input).iter().map(AskedPart::heading).collect();
        assert_eq!(headings, ["Frame · built in", "Task", "Rules", "Status"]);
        let rules = &parts_of(&input)[2].text;
        assert_eq!(
            rules,
            "Decide, don't ask. Nobody reads a question at the end of your turn.\n\n\
             Never change .github/**."
        );
    }

    /// The four parts an agent reads once each is labelled with where it came from.
    fn review() -> SessionInput {
        SessionInput {
            launch: launch(),
            parts: vec![
                part(
                    InputPartKind::Prompt,
                    "frame",
                    None,
                    "You review one change. Judge the change itself.\nYou aren't told who wrote \
                     it. Accept it, or list what must change.",
                ),
                part(
                    InputPartKind::Task,
                    "orchestrator",
                    Some("Opus 5.5"),
                    "Review the due-date change against docs/due.md. Check that\ndates parse as \
                     ISO only and that overdue excludes today.",
                ),
                part(
                    InputPartKind::Rules,
                    "your file",
                    Some("roles/reviewer.md"),
                    "Treat a missing test as a required change, never as a note.",
                ),
                part(
                    InputPartKind::Message,
                    "your request",
                    None,
                    "focus on date parsing",
                ),
            ],
            queued: Vec::new(),
        }
    }

    fn drawn(inputs: &[SessionInput], selected: usize, expanded: Option<usize>) -> String {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let draw = Draw {
            theme: &theme,
            width: 72,
            selected,
            expanded,
        };
        plain_text(&lines(inputs, &draw).lines)
    }

    #[test]
    fn each_part_sits_under_where_it_came_from() {
        assert_eq!(
            drawn(&[review()], 0, None),
            "Frame · built in\n\
             \x20 You review one change. Judge the change itself.\n\
             \x20 You aren't told who wrote it. Accept it, or list what must change.\n\
             \n\
             From the orchestrator · Opus 5.5\n\
             \x20 Review the due-date change against docs/due.md. Check that\n\
             \x20 dates parse as ISO only and that overdue excludes today.\n\
             \n\
             Your file · roles/reviewer.md\n\
             \x20 Treat a missing test as a required change, never as a note.\n\
             \n\
             From your request\n\
             \x20 focus on date parsing\n\
             \n\
             Launched as codex exec --json --sandbox workspace-write '<… · →  expands"
        );
    }

    /// The head an agent's prompt starts with reads as its assembly names each part: the frame,
    /// whoever wrote the instructions, and your file, each with where it came from.
    #[test]
    fn the_head_of_a_prompt_reads_as_its_assembly_names_it() {
        use argonauts_core::prompts::prompt_parts;

        let input = SessionInput {
            launch: launch(),
            parts: vec![
                prompt_parts::frame("You build one task.", None),
                prompt_parts::from_workflow("Build the due field.", "ship · Build".into()),
                prompt_parts::role_file("Keep functions short.", "roles/worker.md".into()),
                prompt_parts::built_in("Report what you did.", None),
            ],
            queued: Vec::new(),
        };
        let headings: Vec<String> = parts_of(&input).iter().map(AskedPart::heading).collect();
        assert_eq!(
            headings,
            [
                "Frame · built in",
                "From the workflow · ship · Build",
                "Your file · roles/worker.md",
                "Instructions · built in",
            ]
        );
    }

    #[test]
    fn todays_parts_keep_their_own_labels_and_the_roles_instructions_are_the_frame() {
        let input = SessionInput {
            launch: launch(),
            parts: vec![
                part(InputPartKind::Prompt, "prompt", None, "You own one task."),
                part(InputPartKind::Task, "task", None, "Add the JSON writer."),
                part(
                    InputPartKind::Notes,
                    "notes",
                    Some(".argonauts/notes/json.md"),
                    "Use serde_json.",
                ),
            ],
            queued: Vec::new(),
        };
        let headings: Vec<String> = parts_of(&input).iter().map(AskedPart::heading).collect();
        assert_eq!(
            headings,
            [
                "Frame · built in",
                "Task",
                "Notes · .argonauts/notes/json.md"
            ]
        );
    }

    #[test]
    fn the_launch_opens_to_the_whole_command_and_where_it_ran() {
        let inputs = [review()];
        let launch_item = items(&inputs) - 1;
        assert_eq!(launch_item, 4);
        let text = drawn(&inputs, launch_item, Some(launch_item));
        assert!(
            text.ends_with(
                "Launched as\n\
                 \x20 $ codex exec --json --sandbox workspace-write '<prompt>'\n\
                 \x20 cwd /wt/task-002 · prompt on stdin · sandbox worktree · sets\n\
                 \x20 ARGONAUTS_SESSION"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_long_part_folds_until_it_is_opened() {
        let long: String = (1..=9).map(|n| format!("line {n}\n")).collect();
        let input = SessionInput {
            launch: launch(),
            parts: vec![part(InputPartKind::Notes, "notes", None, &long)],
            queued: Vec::new(),
        };
        let folded = drawn(std::slice::from_ref(&input), 0, None);
        assert!(
            folded.contains("  line 6\n  … +3 lines · →  expands"),
            "{folded}"
        );
        assert!(!folded.contains("line 7"), "{folded}");
        let open = drawn(&[input], 0, Some(0));
        assert!(open.contains("  line 9"), "{open}");
        assert!(!open.contains("+3 lines"), "{open}");
    }

    #[test]
    fn a_later_delivery_is_headed_as_one() {
        let later = SessionInput {
            launch: launch(),
            parts: vec![part(
                InputPartKind::Message,
                "message",
                Some("you"),
                "use serde_json",
            )],
            queued: Vec::new(),
        };
        let text = drawn(&[review(), later], 0, None);
        assert!(
            text.contains("Added by delivery 1\n\nMessage · you\n  use serde_json"),
            "{text}"
        );
        assert_eq!(text.matches("Launched as").count(), 1, "{text}");
    }

    #[test]
    fn nothing_sent_yet_says_so() {
        assert_eq!(drawn(&[], 0, None), "Nothing sent to this agent yet");
        assert_eq!(items(&[]), 0);
    }

    #[test]
    fn launch_args_paste_back_as_the_same_argv() {
        let args = [
            "claude",
            "-p",
            "it's done",
            "effort=\"low\"",
            "$HOME",
            "a;b",
            "*.rs",
            "x|y",
            "&",
            "`ls`",
            "",
            "plain",
        ];
        let line = args.iter().map(|arg| shell_arg(arg)).collect::<Vec<_>>();
        assert_eq!(line[0], "claude");
        assert_eq!(line[11], "plain");
        assert_eq!(shell_words::split(&line.join(" ")).unwrap(), args);
    }

    #[test]
    fn the_task_reads_in_words_and_the_request_shows_once() {
        let input = SessionInput {
            launch: launch(),
            parts: vec![
                part(
                    InputPartKind::Instructions,
                    "From the orchestrator",
                    Some("Haiku 4.5"),
                    "Add due dates to tasks.\n\n\n\nKeep the CLI's flags as they are.",
                ),
                part(
                    InputPartKind::Task,
                    "task-add-due-dates-tasks",
                    None,
                    "task-add-due-dates-tasks · add due dates\n\nKind: building\nPaths you own:\n- src/due.js",
                ),
                part(
                    InputPartKind::Goal,
                    "goal",
                    Some(".argonauts/goal.md"),
                    "Add due dates to tasks.\n",
                ),
                part(
                    InputPartKind::Rules,
                    "rules",
                    None,
                    "\n\nStay in your paths.\n\n",
                ),
            ],
            queued: Vec::new(),
        };
        let parts = parts_of(&input);
        let headings: Vec<String> = parts.iter().map(AskedPart::heading).collect();
        assert_eq!(
            headings,
            ["From the orchestrator · Haiku 4.5", "Task", "Rules"]
        );
        assert_eq!(
            parts[0].text,
            "Add due dates to tasks.\n\nKeep the CLI's flags as they are."
        );
        assert_eq!(
            parts[1].text,
            "add due dates · building\n\nPaths you own:\n- src/due.js"
        );
        assert_eq!(parts[2].text, "Stay in your paths.");
        assert_eq!(items(std::slice::from_ref(&input)), 4);
    }

    /// A request run whole is the task's title, cut short, and its words after: once, in full.
    #[test]
    fn a_request_run_whole_reads_once_as_its_task() {
        let request =
            "the install command in README.md is wrong, it should be npm install @acme/api";
        let task = format!(
            "task-001 · the install command in README.md is wrong, it should be npm…\n\n\
             Kind: the whole change\n{request}\n\nPaths you own:\n- README.md"
        );
        assert_eq!(
            task_text(&task),
            format!("{request} · the whole change\n\nPaths you own:\n- README.md")
        );
        // A title said again word for word goes once too.
        assert_eq!(
            task_text("task-002 · tidy the changelog\n\nKind: docs\ntidy the changelog"),
            "tidy the changelog · docs"
        );
    }
}
