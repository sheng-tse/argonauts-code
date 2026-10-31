//! Who checks each task a council agreed: the ladder from another maker to a fresh session
//! of the same model, and the scan of added lines for work that signs itself. Only code knows
//! who comes from which maker, so code assigns every check; the seats read "a fresh session
//! that code picks".

use argonauts_core::base::names::{ModelRef, SeatId};
use argonauts_core::runs::events::council::{CheckRung, Seat, SeatRole};
use argonauts_core::settings::council::SameModelCheck;

use super::lexicon::Lexicon;

/// What the ladder gives one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rung {
    /// Whose fresh session checks it. Never a seat's talk session, and never a fork of one.
    pub checker: ModelRef,
    pub rung: CheckRung,
    /// The seat whose model checks, when a seat's does. The check is still a fresh session.
    pub seat: Option<SeatId>,
    /// `council.same_model_check = "ask"` and only the same model can check: you're asked once
    /// per council before such a check goes ahead.
    pub ask_first: bool,
}

impl Rung {
    /// You check the task yourself, as you said when asked about a check by the model that
    /// made it. `checker` stays that model, though no session of it checks.
    pub fn yours(maker: &ModelRef) -> Self {
        Self {
            checker: maker.clone(),
            rung: CheckRung::You,
            seat: None,
            ask_first: false,
        }
    }

    pub fn is_yours(&self) -> bool {
        self.rung == CheckRung::You
    }
}

/// The ladder for one task that `maker` made, from `seats` and every model `set_up` that is
/// ready. The first rung that exists wins:
///
/// 1. Another maker: a seat from another maker, a checker seat first, else any model set up
///    from another maker that isn't a seat, a local open-weight model included.
/// 2. The model that made it, in a fresh session blind to who made the change, as when only
///    one maker is set up.
///
/// Another model of the same maker is never picked by code: a model still favours its own
/// maker's work, and the sibling is usually the weaker reader.
pub fn ladder(
    maker: &ModelRef,
    seats: &[Seat],
    set_up: &[ModelRef],
    provider: &dyn Fn(&ModelRef) -> String,
    same_model_check: SameModelCheck,
) -> Rung {
    let theirs = provider(maker);
    let other = |model: &ModelRef| provider(model) != theirs;
    let mut by_role: Vec<&Seat> = seats.iter().filter(|seat| other(&seat.model)).collect();
    // Stable, so seat order still decides among checkers and among the rest.
    by_role.sort_by_key(|seat| seat.role != SeatRole::Checker);
    if let Some(seat) = by_role.first() {
        return Rung {
            checker: seat.model.clone(),
            rung: CheckRung::OtherMaker,
            seat: Some(seat.id.clone()),
            ask_first: false,
        };
    }
    if let Some(model) = set_up.iter().find(|model| other(model)) {
        return Rung {
            checker: model.clone(),
            rung: CheckRung::OtherMaker,
            seat: None,
            ask_first: false,
        };
    }
    Rung {
        checker: maker.clone(),
        rung: CheckRung::SameMaker,
        seat: None,
        ask_first: same_model_check == SameModelCheck::Ask,
    }
}

/// What a check's own line adds at the same-maker rung.
pub const SAME_MAKER_NOTE: &str = "the same model made it, as no other maker is set up";

/// An added line that signs the work, which sends the task back to its maker before any check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    /// The file the line was added to, when the change says.
    pub file: Option<String>,
    pub line: String,
}

/// The lines a change adds that sign the work: a `Co-Authored-By` trailer, a "Generated with …"
/// line, a robot, or a comment saying which model or tool wrote the code. A model's name in
/// ordinary code stays: only a line that credits one is a signature.
pub fn signatures(diff: &str, lexicon: &Lexicon) -> Vec<Signed> {
    let mut file = None;
    let mut signed = Vec::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            file = Some(path.trim_start_matches("b/").to_owned());
            continue;
        }
        let Some(added) = line.strip_prefix('+') else {
            continue;
        };
        if signs(added, lexicon) {
            signed.push(Signed {
                file: file.clone(),
                line: added.trim().to_owned(),
            });
        }
    }
    signed
}

/// The one line that sends signed work back to its maker.
pub fn send_back_line(signed: &[Signed]) -> String {
    let places: Vec<String> = signed
        .iter()
        .map(|signed| match &signed.file {
            Some(file) => format!("{file}: {}", signed.line),
            None => signed.line.clone(),
        })
        .collect();
    format!(
        "Remove the lines that sign the work, which no change in this council carries: {}",
        places.join(" · ")
    )
}

/// Whether one added line signs the work.
pub fn signs(line: &str, lexicon: &Lexicon) -> bool {
    let lower = line.to_lowercase();
    if lower.contains("co-authored-by:") || line.contains('🤖') {
        return true;
    }
    let names = lexicon.every_name(line);
    if names.is_empty() {
        return false;
    }
    if lower.contains("generated with") || lower.contains("generated by") {
        return true;
    }
    // A comment that credits a name: "// written by GPT-5.6", "# Claude wrote this".
    let Some(comment) = comment_text(line) else {
        return false;
    };
    let comment = comment.to_lowercase();
    const CREDITS: [&str; 8] = [
        "written by",
        "written with",
        "authored by",
        "created by",
        "made by",
        "made with",
        " wrote ",
        "assisted by",
    ];
    CREDITS.iter().any(|credit| comment.contains(credit)) || comment.ends_with(" wrote this")
}

/// The words of a line's comment, when it has one in a common comment form.
fn comment_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let comment = ["//", "#", "--", "/*", "*", "<!--", ";"]
        .iter()
        .find_map(|start| trimmed.strip_prefix(start))
        .or_else(|| line.find(" // ").map(|at| &line[at + 4..]))?;
    let comment = comment
        .trim_start_matches(['/', '*', '!', ' '])
        .trim_end_matches(['*', '/', '-', '>', ' '])
        .trim();
    (!comment.is_empty()).then_some(comment)
}

#[cfg(test)]
mod tests {
    use argonauts_core::runs::events::council::Seat;

    use super::*;

    fn model(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    fn provider(model: &ModelRef) -> String {
        match model.agent.as_str() {
            "claude-code" => "anthropic".to_owned(),
            "codex" => "openai".to_owned(),
            "antigravity-cli" => "google".to_owned(),
            _ => "alibaba".to_owned(),
        }
    }

    fn seat(n: u32, text: &str, role: SeatRole) -> Seat {
        Seat {
            id: SeatId::numbered(n),
            model: model(text),
            role,
            number: None,
        }
    }

    #[test]
    fn another_makers_seat_checks_first_a_checker_seat_before_the_rest() {
        let opus = model("claude-code/claude-opus-5-5");
        let seats = [
            seat(1, "claude-code/claude-opus-5-5", SeatRole::Proposer),
            seat(2, "codex/gpt-5.6-luna", SeatRole::Proposer),
            seat(3, "antigravity-cli/gemini-3.8-flash", SeatRole::Checker),
        ];
        let rung = ladder(&opus, &seats, &[], &provider, SameModelCheck::Allow);
        assert_eq!(rung.rung, CheckRung::OtherMaker);
        assert_eq!(rung.checker, model("antigravity-cli/gemini-3.8-flash"));
        assert_eq!(rung.seat, Some(SeatId::numbered(3)));

        let luna = model("codex/gpt-5.6-luna");
        let plain = [
            seat(1, "claude-code/claude-opus-5-5", SeatRole::Proposer),
            seat(2, "codex/gpt-5.6-luna", SeatRole::Proposer),
        ];
        let rung = ladder(&luna, &plain, &[], &provider, SameModelCheck::Allow);
        assert_eq!(rung.checker, opus, "Opus checks Luna's");
    }

    #[test]
    fn a_model_set_up_from_another_maker_checks_when_no_seat_can() {
        let opus = model("claude-code/claude-opus-5-5");
        let seats = [
            seat(1, "claude-code/claude-opus-5-5", SeatRole::Proposer),
            seat(2, "claude-code/claude-opus-5-5", SeatRole::Skeptic),
        ];
        let local = model("opencode/ollama/qwen3-coder:30b");
        let sonnet = model("claude-code/claude-sonnet-5");
        let rung = ladder(
            &opus,
            &seats,
            &[sonnet.clone(), local.clone()],
            &provider,
            SameModelCheck::Allow,
        );
        assert_eq!(
            (rung.checker, rung.rung, rung.seat),
            (local, CheckRung::OtherMaker, None),
            "a local open-weight model counts, a sibling of the maker's never does"
        );
    }

    #[test]
    fn with_one_maker_a_fresh_session_of_the_same_model_checks_marked_same_maker() {
        let opus = model("claude-code/claude-opus-5-5");
        let seats = [
            seat(1, "claude-code/claude-opus-5-5", SeatRole::Proposer),
            seat(2, "claude-code/claude-opus-5-5", SeatRole::Skeptic),
            seat(3, "claude-code/claude-opus-5-5", SeatRole::Proposer),
        ];
        let sonnet = model("claude-code/claude-sonnet-5");
        let allowed = ladder(&opus, &seats, &[sonnet], &provider, SameModelCheck::Allow);
        assert_eq!(allowed.checker, opus);
        assert_eq!(allowed.rung, CheckRung::SameMaker);
        assert!(!allowed.ask_first);
        let asked = ladder(&opus, &seats, &[], &provider, SameModelCheck::Ask);
        assert!(asked.ask_first, "ask asks you once before such a check");
    }

    #[test]
    fn lines_that_sign_the_work_are_found_and_model_names_in_code_stay() {
        let diff = "\
diff --git a/src/rate/bucket.ts b/src/rate/bucket.ts
+++ b/src/rate/bucket.ts
@@ -1,3 +1,6 @@
+// Written by Claude for the rate limiter
+const model = \"gpt-5.6\"; // the model the docs name
+export const claude = { retries: 2 };
 unchanged line
+++ b/NOTES.md
+Generated with Claude Code
+Co-Authored-By: Someone <someone@example.com>
-// Written by Codex, removed
";
        let found = signatures(diff, &Lexicon::built_in());
        let lines: Vec<(&str, &str)> = found
            .iter()
            .map(|signed| (signed.file.as_deref().unwrap_or(""), signed.line.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                (
                    "src/rate/bucket.ts",
                    "// Written by Claude for the rate limiter"
                ),
                ("NOTES.md", "Generated with Claude Code"),
                ("NOTES.md", "Co-Authored-By: Someone <someone@example.com>"),
            ]
        );
        assert!(send_back_line(&found).starts_with("Remove the lines that sign the work"));
    }

    #[test]
    fn a_signature_counts_even_when_the_work_is_about_the_name() {
        let lexicon = Lexicon::built_in().with_topic("Retry in the Codex adapter");
        assert!(signs("// Written by Codex for the retries", &lexicon));
        assert!(!signs("// The Codex adapter retries twice", &lexicon));
    }
}
