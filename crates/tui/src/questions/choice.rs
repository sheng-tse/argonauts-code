//! App choices as questions: the unclear request, the scope and safety questions the controller
//! asks, trust, check commands, rewind, and the guided setup's steps. Each has exactly the
//! options the controller sent, and its hint line says what Esc does for its kind.

use argonauts_core::app::{AppChoice, ChoiceKind};
use argonauts_core::runs::events::approval::{AnswerAction, FreeText};

use super::options::{first_recommended, question_options};
use crate::bottom::QuestionPrompt;

/// Rows of a choice's body shown on the question. The rest waits for Show the file.
const MAX_BODY_ROWS: usize = 8;
/// Rows of a dry run's table, which is all the question is for.
const MAX_TABLE_ROWS: usize = 24;

/// A setup is what its question asks about, so it may run longer.
const MAX_SETUP_ROWS: usize = 16;

/// The cells between the longest answer to a needed update and what each answer does.
const NEEDED_UPDATE_GAP: usize = 4;

/// The cells between the longest name `/templates` lists and what kind of thing it is.
const TEMPLATES_GAP: usize = 4;

/// The cells between a setup step's longest answer and what each answer means.
const SETUP_GAP: usize = 4;

/// The cells between Rename them and Leave them and what each does.
const RENAMED_GAP: usize = 3;

pub fn choice_question(id: u64, choice: &AppChoice) -> QuestionPrompt {
    let options = question_options(&choice.options, false);
    let mut question = QuestionPrompt::new(id, choice.title.clone(), choice.title.clone(), options);
    question.selected = first_recommended(&choice.options);
    // A trust question starts on the safe answer, as Claude Code's trust dialog starts on
    // "No, exit", and Enter confirms what the pointer is on.
    if choice.kind == ChoiceKind::TrustProject {
        if let Some(not_now) = choice
            .options
            .iter()
            .position(|option| option.action == AnswerAction::Cancel)
        {
            question.selected = not_now;
        }
        question.enter_hint = Some("Enter to confirm".to_owned());
        // A file that sets safety settings you haven't trusted is a warning, as its title says.
        question.warns = true;
    }
    question.allow_note = choice.free_text != FreeText::None;
    question.esc_hint = esc_hint(choice.kind).to_owned();
    // A preview runs only on its own key, y.
    question.keys_only = choice.kind == ChoiceKind::TryWorkflow;
    // Every choice asks you, so every one takes the keys. What is different is who set it off. Work
    // raised these after the main agent's or the CLI's reply, which you may rather answer by
    // typing, so Esc hands them on and gives you the input. You set off the rest, so they open
    // with the keys and Esc closes them.
    if raised_by_work(choice.kind) {
        question.needs_you = true;
    } else {
        question = question.yours();
        question.esc_answers = true;
    }

    let mut rows: Vec<String> = choice.body.clone();
    let most = match choice.kind {
        ChoiceKind::TryWorkflow => MAX_TABLE_ROWS,
        _ => MAX_BODY_ROWS,
    };
    if rows.len() > most {
        let hidden = rows.len() - (most - 1);
        rows.truncate(most - 1);
        rows.push(format!("… +{hidden} more lines"));
    }
    match choice.kind {
        // Keys and commands read best framed, the way Claude Code frames what it asks about.
        ChoiceKind::ConfirmSafetySetting
        | ChoiceKind::TrustProject
        | ChoiceKind::DetectedChecks
        | ChoiceKind::ConfirmRewind
        | ChoiceKind::ConfirmConnect
        | ChoiceKind::Permission
        | ChoiceKind::TryWorkflow => question.body = rows,
        ChoiceKind::StartWork
        | ChoiceKind::RefusedCommands
        | ChoiceKind::GoAhead
        | ChoiceKind::Fallback
        | ChoiceKind::RewindTurn
        | ChoiceKind::ConfirmTakeOver
        | ChoiceKind::TermsNote
        | ChoiceKind::SettingScope
        | ChoiceKind::KeepInstructions
        | ChoiceKind::Git => question.detail = rows,
        ChoiceKind::Init => {
            question.detail = rows;
            question.framed = true;
            question.descriptions_beside = Some(SETUP_GAP);
        }
        ChoiceKind::RenamedSettings => {
            question.noted_rows = choice.body.iter().take(MAX_SETUP_ROWS).cloned().collect();
            question.two_columns = true;
            question.framed = true;
            question.descriptions_beside = Some(RENAMED_GAP);
        }
        // Many names to pick from, so each takes one row with its kind beside it.
        ChoiceKind::Templates => {
            question.detail = rows;
            question.descriptions_beside = Some(TEMPLATES_GAP);
        }
        ChoiceKind::NeededUpdate => {
            question.detail = rows;
            question.descriptions_beside = Some(NEEDED_UPDATE_GAP);
        }
        ChoiceKind::Setup => {
            question.noted_rows = setup_rows(&choice.body);
            // Yes and Not now are listed, and `d` shows what would be written.
            if let Some(last) = choice.options.last()
                && last.action == AnswerAction::ShowDiff
            {
                question.options.pop();
                question.view_key = Some((last.key, choice.options.len() - 1));
            }
        }
    }
    crate::views::issues::shape_question(&mut question, choice);
    question
}

/// A setup's question shows the setup, which ends at the first blank line. What yes would write
/// follows it, for Show what would be written.
fn setup_rows(body: &[String]) -> Vec<String> {
    let mut rows: Vec<String> = body
        .iter()
        .take_while(|line| !line.trim().is_empty())
        .cloned()
        .collect();
    if rows.len() > MAX_SETUP_ROWS {
        let hidden = rows.len() - (MAX_SETUP_ROWS - 1);
        rows.truncate(MAX_SETUP_ROWS - 1);
        rows.push(format!("… +{hidden} more lines"));
    }
    rows
}

/// Work raised it after a reply in the conversation, rather than a command or key of yours. A CLI's
/// permission prompt counts as yours, since Esc answers it No, as in Claude Code.
fn raised_by_work(kind: ChoiceKind) -> bool {
    match kind {
        ChoiceKind::StartWork
        | ChoiceKind::RefusedCommands
        | ChoiceKind::GoAhead
        | ChoiceKind::Fallback
        | ChoiceKind::NeededUpdate => true,
        ChoiceKind::Permission
        | ChoiceKind::ConfirmTakeOver
        | ChoiceKind::ConfirmSafetySetting
        | ChoiceKind::TermsNote
        | ChoiceKind::Init
        | ChoiceKind::SettingScope
        | ChoiceKind::TrustProject
        | ChoiceKind::DetectedChecks
        | ChoiceKind::ConfirmRewind
        | ChoiceKind::ConfirmConnect
        | ChoiceKind::KeepInstructions
        | ChoiceKind::RewindTurn
        | ChoiceKind::TryWorkflow
        | ChoiceKind::Setup
        | ChoiceKind::Git
        | ChoiceKind::Templates
        | ChoiceKind::RenamedSettings => false,
    }
}

/// What the controller does when you press Esc, by kind.
pub fn esc_hint(kind: ChoiceKind) -> &'static str {
    match kind {
        ChoiceKind::StartWork | ChoiceKind::KeepInstructions | ChoiceKind::RefusedCommands => {
            crate::bottom::ESC_HINT
        }
        ChoiceKind::Permission | ChoiceKind::GoAhead => "Esc for No",
        ChoiceKind::Fallback => "Esc to stop",
        ChoiceKind::ConfirmTakeOver
        | ChoiceKind::SettingScope
        | ChoiceKind::ConfirmSafetySetting
        | ChoiceKind::ConfirmRewind
        | ChoiceKind::RewindTurn
        | ChoiceKind::Git
        | ChoiceKind::Templates
        | ChoiceKind::NeededUpdate => "Esc to cancel",
        ChoiceKind::TrustProject
        | ChoiceKind::DetectedChecks
        | ChoiceKind::ConfirmConnect
        | ChoiceKind::Setup => "Esc for not now",
        ChoiceKind::TermsNote => "Esc to take the recommended answers",
        // Esc takes the recommended answers on a setup step too, and its key line names only
        // Enter, as Claude Code's first-run steps do.
        ChoiceKind::Init => "",
        ChoiceKind::RenamedSettings => "Esc to leave them",
        ChoiceKind::TryWorkflow => "y runs it · n closes it",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::base::names::ChoiceId;
    use argonauts_core::runs::events::approval::AnswerOption;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use super::*;
    use crate::base::draw::render::{Clock, RenderCtx};
    use crate::base::text::plain_text;
    use crate::testing::pictures::golden::{Picture, Region, differences, theme};

    fn answer(key: char, label: &str, value: &str, detail: &str) -> AnswerOption {
        AnswerOption {
            key,
            label: label.to_owned(),
            action: AnswerAction::Choose {
                value: value.to_owned(),
            },
            recommended: false,
            detail: Some(detail.to_owned()),
        }
    }

    /// The versions picture's example: Codex 0.158.0 from Homebrew, older than the oldest
    /// version supported.
    fn needed_update() -> AppChoice {
        AppChoice {
            id: ChoiceId::numbered(1),
            kind: ChoiceKind::NeededUpdate,
            title: "Codex 0.158.0 is older than Argonauts Code supports (0.160.0)".to_owned(),
            body: vec![
                "Runs with it may misread what it does. Nothing has started yet.".to_owned(),
            ],
            options: vec![
                answer(
                    'u',
                    "Update it now",
                    "update",
                    "brew upgrade codex, then the run starts",
                ),
                answer(
                    'l',
                    "Leave Codex out of this run",
                    "leave_out",
                    "its roles go to the next model in your fallback order",
                ),
                answer(
                    'r',
                    "Run with it anyway",
                    "run",
                    "and ask again before the next run",
                ),
            ],
            free_text: FreeText::None,
            bell: false,
        }
    }

    fn drawn(question: &QuestionPrompt, width: u16) -> Buffer {
        let theme = theme();
        let lines = question.lines(&RenderCtx::new(width, &theme, Clock::still(Instant::now())));
        let mut buf = Buffer::empty(Rect::new(0, 0, width, lines.len() as u16));
        for (y, line) in lines.iter().enumerate() {
            buf.set_line(0, y as u16, line, width);
        }
        buf
    }

    /// The question at the picture's width, from its rule to its last answer: every character,
    /// and every colour but the three the frame all questions share draws its own way. That
    /// frame has its rule and title in the question's colour and its numbers dim, where the
    /// picture has a faint rule, a plain title and numbers in their answer's colour.
    #[test]
    fn the_needed_update_question_matches_its_picture() {
        const RULE: usize = 12;
        const TITLE: usize = 13;
        const ANSWERS: std::ops::Range<usize> = 16..19;
        const NUMBERS: std::ops::Range<usize> = 4..6;

        let shot = Picture::load("versions");
        let mut question = choice_question(1, &needed_update());
        question.focus();
        let buf = drawn(&question, shot.width() as u16);
        let region = |rows: std::ops::Range<usize>, columns: std::ops::Range<usize>| Region {
            at: (0, (rows.start - RULE) as u16),
            rows,
            columns,
        };
        let different =
            |region: &Region| -> Vec<String> { differences(&shot, &buf, region, &theme()) };

        let width = shot.width();
        let mut found = different(&region(TITLE + 1..ANSWERS.start, 0..width));
        found.extend(different(&region(ANSWERS, 0..NUMBERS.start)));
        found.extend(different(&region(ANSWERS, NUMBERS.end..width)));
        assert!(found.is_empty(), "{}", found.join("\n"));

        // The frame's own cells still hold the picture's characters.
        let frame = [region(RULE..TITLE + 1, 0..width), region(ANSWERS, NUMBERS)];
        for cells in &frame {
            let found = different(cells);
            let colours_only = found.iter().all(|cell| cell.contains(": colour "));
            assert!(colours_only, "{}", found.join("\n"));
        }
        // And the marker before the title is the picture's.
        assert!(different(&region(TITLE..TITLE + 1, 0..2)).is_empty());
    }

    /// What `/templates` asks once it has listed them: twelve workflows and five roles'
    /// prompts to pick one from.
    fn which_template() -> AppChoice {
        const WORKFLOWS: [&str; 12] = [
            "Council",
            "Orchestrator",
            "Spec-first",
            "Maker-checker",
            "Best-of-n",
            "Draft-and-vote",
            "Ablate-no-blind",
            "Ablate-no-anonymize",
            "Ablate-no-critique",
            "Ablate-fixed-roles",
            "Ablate-same-provider",
            "Ablate-no-experiments",
        ];
        const PROMPTS: [&str; 5] = ["main", "orchestrator", "workers", "reviewers", "judge"];
        let kinds = WORKFLOWS
            .iter()
            .map(|name| (*name, "workflow"))
            .chain(PROMPTS.iter().map(|name| (*name, "prompts")));
        let options = kinds
            .zip("123456789abcdfghi".chars())
            .map(|((name, kind), key)| match name {
                "Council" => answer(key, name, name, "workflow · in use"),
                _ => answer(key, name, name, kind),
            })
            .collect();
        AppChoice {
            id: ChoiceId::numbered(1),
            kind: ChoiceKind::Templates,
            title: "Copy which one?".to_owned(),
            body: Vec::new(),
            options,
            free_text: FreeText::None,
            bell: false,
        }
    }

    fn rows_within(question: &QuestionPrompt, width: u16, rows: usize) -> Vec<String> {
        let theme = theme();
        let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
        plain_text(&question.layout_within(&ctx, rows).lines)
            .lines()
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    /// Seventeen names are more than a question lists at once. Each takes one row with its
    /// kind beside it, ten show around the pointer, and the key line says which.
    #[test]
    fn the_templates_question_lists_ten_names_a_row_each_around_the_pointer() {
        let mut question = choice_question(1, &which_template());
        let rows = rows_within(&question, 80, usize::MAX);
        assert_eq!(rows.len(), 15, "{rows:#?}");
        assert_eq!(rows[1], "◆ Copy which one?");
        assert_eq!(rows[2], "");
        assert_eq!(
            rows[3],
            "  ❯ 1.  Council                  workflow · in use"
        );
        assert_eq!(rows[4], "    2.  Orchestrator             workflow");
        assert_eq!(rows[12], "    10. Ablate-fixed-roles       workflow");
        assert_eq!(rows[13], "");
        let keys = "  Enter to choose · Esc to cancel";
        assert_eq!(rows[14], format!("{keys:<64}  1–10 of 17 ↓"));
        assert_eq!(crate::base::text::str_width(&rows[14]), 78);

        // The pointer moves through the first half of the window, then the window follows it.
        let down = |question: &mut QuestionPrompt, times: usize| {
            for _ in 0..times {
                question.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            }
        };
        down(&mut question, 5);
        let rows = rows_within(&question, 80, usize::MAX);
        assert_eq!(
            rows[3],
            "    1.  Council                  workflow · in use"
        );
        assert_eq!(rows[8], "  ❯ 6.  Draft-and-vote           workflow");
        down(&mut question, 5);
        let rows = rows_within(&question, 80, usize::MAX);
        assert_eq!(rows[3], "    6.  Draft-and-vote           workflow");
        assert_eq!(rows[8], "  ❯ 11. Ablate-same-provider     workflow");
        assert_eq!(rows[12], "    15. workers                  prompts");
        assert_eq!(rows[14], format!("{keys:<63}↑  6–15 of 17 ↓"));
        down(&mut question, 6);
        let rows = rows_within(&question, 80, usize::MAX);
        assert_eq!(rows[3], "    8.  Ablate-no-anonymize      workflow");
        assert_eq!(rows[12], "  ❯ 17. judge                    prompts");
        assert_eq!(rows[14], format!("{keys:<63}↑  8–17 of 17"));
    }

    /// On a screen with fewer rows than the ten names need, the window shrinks and the rule,
    /// the title and the key line stay: the blank rows go first.
    #[test]
    fn a_short_screen_shrinks_the_templates_window_and_keeps_its_title_and_keys() {
        let mut question = choice_question(1, &which_template());
        for _ in 0..8 {
            question.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let keys = "  Enter to choose · Esc to cancel";
        // One row short: the blank row under the title goes.
        let rows = rows_within(&question, 80, 14);
        assert_eq!(rows.len(), 14, "{rows:#?}");
        assert_eq!(rows[1], "◆ Copy which one?");
        assert_eq!(rows[2], "    4.  Maker-checker            workflow");
        assert_eq!(rows[12], "");
        assert_eq!(rows[13], format!("{keys:<63}↑  4–13 of 17 ↓"));
        // Then the one over the keys.
        let rows = rows_within(&question, 80, 13);
        assert_eq!(rows.len(), 13, "{rows:#?}");
        assert_eq!(rows[11], "    13. main                     prompts");
        assert_eq!(rows[12], format!("{keys:<63}↑  4–13 of 17 ↓"));
        // Then the window takes fewer names, the pointer still among them.
        let rows = rows_within(&question, 80, 8);
        assert_eq!(
            rows,
            [
                "─".repeat(80).as_str(),
                "◆ Copy which one?",
                "    7.  Ablate-no-blind          workflow",
                "    8.  Ablate-no-anonymize      workflow",
                "  ❯ 9.  Ablate-no-critique       workflow",
                "    10. Ablate-fixed-roles       workflow",
                "    11. Ablate-same-provider     workflow",
                format!("{keys:<63}↑  7–11 of 17 ↓").as_str(),
            ],
        );
        // However few the rows, one name shows between the title and the keys.
        let rows = rows_within(&question, 80, 3);
        assert_eq!(
            rows[1..],
            [
                "◆ Copy which one?",
                "  ❯ 9.  Ablate-no-critique       workflow",
                format!("{keys:<64}↑  9–9 of 17 ↓").as_str(),
            ],
        );
    }

    /// Where a row has no room for an answer and what it does side by side, each answer's note
    /// goes under it, as on every other question.
    #[test]
    fn a_narrow_screen_puts_each_note_under_its_answer() {
        let mut question = choice_question(1, &needed_update());
        question.focus();
        let theme = theme();
        let lines = question.lines(&RenderCtx::new(60, &theme, Clock::still(Instant::now())));
        let text = plain_text(&lines);
        assert!(
            text.contains("  ❯ 1. Update it now\n       brew upgrade codex, then the run starts\n"),
            "{text}"
        );
    }
}
