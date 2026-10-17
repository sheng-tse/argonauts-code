//! What a stretch of work may show under the working line as `⎿ Tip: …`, as Claude Code's
//! spinner does, while `ui.tips` is on. Each names a key or command Argonauts Code has with its
//! default binding.

pub const TIPS: &[&str] = &[
    "Shift+Tab cycles the approval mode: supervised, auto, autopilot",
    "Type ultracode in a prompt to send that request through a workflow",
    "← on an empty prompt opens the agent panel",
    "Ctrl+T shows the workflow a run follows",
    "Ctrl+O shows the whole transcript",
    "Ctrl+R searches the prompts you sent before",
    "Ctrl+G writes the prompt in your editor",
    "! at the start of the input runs a shell command",
    "/config changes your settings, and /config all lists every key",
    "/output-style picks how answers are written",
    "/fast answers at the lowest effort the model takes",
    "/resume picks up an earlier conversation",
    "/usage shows each CLI's plan limits and when they reset",
    "/mods picks what the bottom line shows",
    "? on an empty prompt lists every shortcut",
];

#[cfg(test)]
mod tests {
    use super::TIPS;

    #[test]
    fn tips_are_one_short_line_each() {
        for tip in TIPS {
            assert!(tip.chars().count() <= 70, "{tip}");
            assert!(!tip.ends_with('.'), "{tip}");
        }
    }
}
