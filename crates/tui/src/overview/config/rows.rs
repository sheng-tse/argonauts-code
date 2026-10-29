//! The Config tab's rows: each a label, the key it reads and saves, and how it changes. The
//! rows you reach for most come first, then the rest of Claude Code's list in its order, then
//! Argonauts Code's own, and last the way to every key.

use super::picks::Pick;

/// How a row changes, as Claude Code's Config draws each kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// `true` or `false`: Enter, Space, ← and → flip it.
    Toggle(Flip),
    /// One of a few values, `‹ value ›` while the pointer is on it: Enter, Space and → step
    /// on, ← back. With no values of its own it steps through the ones the key allows.
    Choice(&'static [Choice]),
    /// `value ›`: Enter opens the command's own picker, as `/theme` and `/model` do.
    Opens(&'static str),
    /// `value ›`: Enter opens a picker over the rows of the models the deeper Behaviors map may
    /// use, from the offer `d` asks with.
    Picks(Pick),
    /// A value you type, edited as All settings edits a key.
    Typed,
    /// Whether a card shows on the bottom line, which keeps a file of its own.
    OnTheLine(&'static str),
    /// Every key, with where each came from.
    AllSettings,
}

/// What a toggle writes on and off, for a key that may hold a word rather than true or false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flip {
    pub on: &'static str,
    pub off: &'static str,
    /// Whether a value that is neither reads as on, as `inherit` does for auto-compact.
    pub others_on: bool,
}

impl Flip {
    pub fn reads_on(&self, value: &str) -> bool {
        let value = value.trim();
        value == self.on || (self.others_on && value != self.off)
    }
}

const BOOL: Control = Control::Toggle(Flip {
    on: "true",
    off: "false",
    others_on: false,
});

/// A value of a choice as TOML text, and its words on the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub toml: &'static str,
    pub words: &'static str,
}

const fn choice(toml: &'static str, words: &'static str) -> Choice {
    Choice { toml, words }
}

/// Steps through the values the key allows, each in its own words.
const ALLOWED: Control = Control::Choice(&[]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub label: &'static str,
    /// Empty for a row that holds no key of its own.
    pub key: &'static str,
    pub control: Control,
}

const fn row(label: &'static str, key: &'static str, control: Control) -> Row {
    Row {
        label,
        key,
        control,
    }
}

/// What a row's values mean, dim after its value, for a key whose words alone don't say.
pub fn described(key: &str, value: &str) -> Option<&'static str> {
    let auto = matches!(value.trim().trim_matches('"'), "" | "auto");
    match key {
        "behaviors.map" => Some("labels, or labels and summaries"),
        "behaviors.summarizer" if auto => {
            Some("a model on this computer, else your main CLI's cheapest")
        }
        "behaviors.embedder" if auto => Some("nomic-embed-text on Ollama, else word vectors"),
        _ => None,
    }
}

/// The key Default approval mode saves, which says Last used with the mode it brings back.
pub const START_MODE: &str = "approvals.start_mode";

pub const ROWS: &[Row] = &[
    row(
        "Auto-compact",
        "compaction",
        Control::Toggle(Flip {
            on: "\"auto\"",
            off: "\"off\"",
            others_on: true,
        }),
    ),
    row(
        "Continue at a usage limit",
        "fallback.mode",
        Control::Choice(&[
            choice("\"auto\"", "fall back, then wait"),
            choice("\"ask\"", "ask me first"),
            choice("\"wait\"", "wait for the limit"),
        ]),
    ),
    row("Show tips", "ui.tips", BOOL),
    row(
        "Reduce motion",
        "ui.motion",
        Control::Toggle(Flip {
            on: "\"reduced\"",
            off: "\"normal\"",
            others_on: false,
        }),
    ),
    row("Session recap", "ui.recaps", BOOL),
    row("Rewind code (checkpoints)", "captain.rewind", BOOL),
    row(
        "Verbose output",
        "ui.verbosity",
        Control::Toggle(Flip {
            on: "\"full\"",
            off: "\"normal\"",
            others_on: false,
        }),
    ),
    row("Show turn duration", "ui.turn_duration", BOOL),
    row(
        "Time format",
        "ui.format.clock",
        Control::Choice(&[
            choice("\"auto\"", "auto"),
            choice("\"12h\"", "12h"),
            choice("\"24h\"", "24h"),
        ]),
    ),
    row(
        "Default approval mode",
        START_MODE,
        Control::Choice(&[
            choice("\"last\"", "Last used"),
            choice("\"approvals\"", "as your approvals say"),
            choice("\"supervised\"", "supervised"),
            choice("\"auto_preview\"", "auto preview"),
            choice("\"auto\"", "auto"),
            choice("\"autopilot\"", "autopilot"),
        ]),
    ),
    row("Theme", "ui.theme", Control::Opens("theme")),
    row(
        "Editor mode",
        "ui.keys",
        Control::Choice(&[choice("\"default\"", "normal"), choice("\"vim\"", "vim")]),
    ),
    row("Model", "crew.main", Control::Opens("model")),
    row("Effort", "roles.main.effort", Control::Opens("effort")),
    row("Workflow", "workflow", Control::Opens("workflow")),
    row(
        "Reviewer",
        "crew.reviewer",
        Control::Opens("model reviewer"),
    ),
    row("Judge", "crew.judge", Control::Opens("model judge")),
    row("Fallback", "fallback.same_cli", Control::Opens("fallback")),
    row("Spend per run", "limits.spend_per_run", Control::Typed),
    row(
        "Usage on the bottom line",
        "",
        Control::OnTheLine("plan-usage"),
    ),
    // The rest of Claude Code's list, in its order.
    row("Thinking mode", "main_session.thinking", BOOL),
    row("Fast mode", "main_session.fast", BOOL),
    row("Prompt suggestions", "ui.suggestions", BOOL),
    row("Dynamic workflows", "crew.worker_workflows", BOOL),
    row(
        "Ultracode keyword trigger",
        "workflows.ultracode_trigger",
        BOOL,
    ),
    row("Dynamic workflow size", "workflow_size", ALLOWED),
    row("Terminal progress bar", "ui.progress_bar", BOOL),
    row("Copy on select", "ui.copy_on_select", BOOL),
    row("Auto-scroll", "ui.autoscroll", BOOL),
    row(
        "Auto-update channel",
        "updates.channel",
        Control::Choice(&[choice("\"stable\"", "stable"), choice("\"beta\"", "beta")]),
    ),
    row("Open agents view by default", "ui.agents_view", BOOL),
    row("Notifications", "notifications.os", BOOL),
    row("Output style", "ui.output_style", ALLOWED),
    row("Language", "main_session.language", Control::Typed),
    row(
        "Question auto-continue timeout",
        "fallback.ask_timeout",
        Control::Typed,
    ),
    row("Show PR status footer", "ui.pr_footer", BOOL),
    // Argonauts Code's own.
    row(
        "Orchestrator",
        "crew.orchestrator",
        Control::Opens("model orchestrator"),
    ),
    row("Worker", "crew.worker", Control::Opens("model worker")),
    row("Merger", "crew.merger", Control::Opens("model merger")),
    row("Approvals", "", Control::Opens("approvals")),
    row(
        "Who answers agents' questions",
        "approvals.answers",
        Control::Typed,
    ),
    row("Spend per day", "limits.spend_per_day", Control::Typed),
    row("Sandbox", "sandbox.mode", Control::Opens("permissions")),
    row("Network", "sandbox.network", BOOL),
    row("Agents commit", "sandbox.agents_commit", BOOL),
    row("Commits", "delivery.commits", ALLOWED),
    row("Git", "", Control::Opens("git")),
    row("Narrator", "ui.narrator", Control::Opens("narrator")),
    row("Spinner verbs", "ui.verbs", ALLOWED),
    row("Glyphs", "ui.glyphs", ALLOWED),
    row(
        "Agents in the conversation",
        "ui.agents_in_conversation",
        ALLOWED,
    ),
    row("Newer CLI notice", "ui.newer_cli_notice", BOOL),
    row("Mods", "", Control::Opens("mods")),
    row("Bottom line", "", Control::Opens("mods bottom-line")),
    row("Keep the Mac awake", "awake.while_working", BOOL),
    row(
        "Behaviors map",
        "behaviors.map",
        Control::Choice(&[
            choice("\"labels\"", "labels"),
            choice("\"deeper\"", "deeper"),
        ]),
    ),
    row(
        "Behaviors summarizer",
        "behaviors.summarizer",
        Control::Picks(Pick::Summarizer),
    ),
    row(
        "Behaviors embedder",
        "behaviors.embedder",
        Control::Picks(Pick::Embedder),
    ),
    row("Auto-updates", "updates.auto", BOOL),
    row("Usage reports", "telemetry.enabled", BOOL),
    row("All settings", "", Control::AllSettings),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The channel sits where Claude Code's own is, after Auto-scroll, and whether Argonauts
    /// Code updates itself sits with Argonauts Code's own rows.
    #[test]
    fn updates_have_their_rows() {
        let at = |key: &str| ROWS.iter().position(|row| row.key == key).unwrap();
        let channel = ROWS[at("updates.channel")];
        assert_eq!(at("updates.channel"), at("ui.autoscroll") + 1);
        assert_eq!(channel.label, "Auto-update channel");
        let Control::Choice(values) = channel.control else {
            panic!("{channel:?}");
        };
        let words: Vec<&str> = values.iter().map(|value| value.words).collect();
        assert_eq!(words, ["stable", "beta"]);
        let auto = ROWS[at("updates.auto")];
        assert_eq!((auto.label, auto.control), ("Auto-updates", BOOL));
    }
}
