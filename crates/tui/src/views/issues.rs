//! `/issue`'s list of the repository's open issues, drawn in the input's place as `/model` draws
//! its list: yours first, each with its number, its labels and how long ago it changed. Enter
//! starts on the issue under the pointer.

use argonauts_core::app::{AppChoice, IssueRow, UiCommand, ViewData};
use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalOpened, ApprovalPacket,
};
use argonauts_core::input::{IssueRef, SlashCommand};
use time::OffsetDateTime;

use crate::bottom::{Picker, PickerEvent, PickerRow, QuestionPrompt};
use crate::bridge::Cx;
use crate::cell::UserMessage;

/// The command whose picker this is, which keys find it by.
pub(crate) const COMMAND: &str = "issue";

/// What every answer of `/issue`'s own questions chooses starts with this.
const ANSWERS: &str = "issue:";

/// The cells between the longest answer to one of `/issue`'s questions and what each does.
const NOTES_GAP: usize = 3;

const ABOUT: &str =
    "Open issues, yours first. A run reads the issue and works on branch issue-<n>-…";

/// Opens the list an `Issues` answer holds, or says why there is none, under the command's echo.
pub(super) fn open(data: &ViewData, cx: &mut Cx<'_>) {
    let ViewData::Issues {
        repo,
        issues,
        problem,
        open,
    } = data
    else {
        return;
    };
    let mut picker = picker(repo, issues, problem.as_deref(), OffsetDateTime::now_utc());
    if let Some(open) = open {
        picker.about = cut_about(issues.len(), *open);
    }
    cx.model
        .history
        .push(UserMessage::command(format!("/{COMMAND}")));
    picker.flow.echoed_at = Some(cx.model.history.cells().len());
    picker.vim = cx.model.bottom.mode.vim.is_some();
    cx.model.bottom.picker = Some(picker);
}

/// Whether the open picker is `/issue`'s.
pub(crate) fn owns(cx: &Cx<'_>) -> bool {
    cx.model
        .bottom
        .picker
        .as_ref()
        .is_some_and(|picker| picker.flow.command == COMMAND)
}

/// A key the list took: Enter starts on the issue under the pointer, and Esc closes it.
pub(crate) fn on_picker(event: &PickerEvent, cx: &mut Cx<'_>) {
    match event {
        PickerEvent::Picked(index) => {
            let number = cx
                .model
                .bottom
                .picker
                .as_ref()
                .and_then(|picker| picker.rows.get(*index))
                .and_then(number_of);
            cx.model.bottom.picker = None;
            if let Some(number) = number {
                cx.out.send(UiCommand::Slash {
                    command: SlashCommand::Issue {
                        issue: Some(IssueRef {
                            number,
                            url: None,
                            all_comments: false,
                        }),
                    },
                });
            }
        }
        PickerEvent::Closed => cx.model.bottom.picker = None,
        _ => {}
    }
}

fn number_of(row: &PickerRow) -> Option<u64> {
    row.number.as_deref()?.strip_prefix('#')?.parse().ok()
}

/// `/issue`'s own questions, known by their answers, draw as the merge question does: what
/// each answer does beside it, and Esc sets the question aside to answer later.
pub(crate) fn shape_question(question: &mut QuestionPrompt, choice: &AppChoice) {
    if ours(&choice.options) {
        as_an_issue_question(question);
    }
}

/// The draft a run on an issue ends with, asked before anything goes to GitHub: its title,
/// then two dim lines on the branch and who it posts as, then the draft's title and body, each
/// after its dim name.
pub(crate) fn shape_approval(question: &mut QuestionPrompt, opened: &ApprovalOpened) {
    let ApprovalPacket::Question(asked) = &opened.packet else {
        return;
    };
    if !ours(&opened.options) {
        return;
    }
    as_an_issue_question(question);
    opened.title.clone_into(&mut question.title);
    opened.title.clone_into(&mut question.short_title);
    question.agent = None;
    question.question = None;
    // Autopilot never posts in your place here unasked, so Shift+Tab isn't offered.
    question.offers_shift_tab = false;
    let mut lines = asked.text.lines();
    question.detail = lines
        .by_ref()
        .take_while(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect();
    question.noted_rows = std::iter::once(String::new())
        .chain(lines.map(named_row))
        .collect();
}

/// Whether Esc only sets this run question aside: a question of `/issue`'s posts on GitHub as
/// you, so it waits for your own answer and is never left to anyone.
pub(crate) fn waits_for_you(opened: &ApprovalOpened) -> bool {
    ours(&opened.options)
}

fn ours(options: &[AnswerOption]) -> bool {
    options.iter().any(|option| {
        matches!(&option.action, AnswerAction::Choose { value } if value.starts_with(ANSWERS))
    })
}

fn as_an_issue_question(question: &mut QuestionPrompt) {
    question.descriptions_beside = Some(NOTES_GAP);
    question.esc_hint = "Esc to set aside".to_owned();
}

/// The columns the draft's rows give their names, `title   ` and `body    `.
const NAMES: usize = 8;

/// `title   Give each API key…` as a noted row: the name dim, the value plain, and a folded
/// part of the body dim.
fn named_row(line: &str) -> String {
    let split = line
        .char_indices()
        .nth(NAMES)
        .map_or(line.len(), |(at, _)| at);
    let (name, value) = line.split_at(split);
    if value.starts_with('…') {
        format!("\t{name}\t\t{value}")
    } else {
        format!("\t{name}\t{value}")
    }
}

/// The line under the title when the list holds only some of the open issues: `32 of 40 open
/// issues, yours first · /issue <number> starts any`.
fn cut_about(listed: usize, open: usize) -> String {
    format!("{listed} of {open} open issues, yours first · /issue <number> starts any")
}

/// The list as the picker draws it. With a problem, or no open issue, it says so in place of
/// the rows.
pub(crate) fn picker(
    repo: &str,
    issues: &[IssueRow],
    problem: Option<&str>,
    now: OffsetDateTime,
) -> Picker {
    let title = if repo.is_empty() {
        "Issues".to_owned()
    } else {
        format!("Issues · {repo}")
    };
    let mut picker = Picker::new(title, ABOUT);
    picker.flow.command = COMMAND.to_owned();
    picker.enter_hint = "Enter to start".to_owned();
    picker.wide_labels = true;
    picker.placeholder = match problem {
        Some(problem) => problem.to_owned(),
        None => format!("No open issues in {repo}."),
    };
    if problem.is_some() {
        return picker;
    }
    // Headings only part two groups.
    let grouped = issues.iter().any(|issue| issue.yours) && issues.iter().any(|issue| !issue.yours);
    let rows = issues
        .iter()
        .enumerate()
        .map(|(at, issue)| {
            let starts_group = at == 0 || issues[at - 1].yours != issue.yours;
            let heading = (grouped && starts_group)
                .then(|| if issue.yours { "Yours" } else { "Others" }.to_owned());
            let number = format!("#{}", issue.number);
            PickerRow {
                heading,
                also_matches: Some(number.clone()),
                number: Some(number),
                aside: Some(ago(issue.updated, now)),
                ..PickerRow::new(issue.title.clone(), issue.labels.join(", "))
            }
        })
        .collect();
    picker.set_rows(rows);
    picker
}

/// `5h ago`, `1d ago`, `3w ago`, as the list says how long ago an issue last changed.
fn ago(at: OffsetDateTime, now: OffsetDateTime) -> String {
    let seconds = (now - at).whole_seconds().max(0);
    let (amount, unit) = match seconds {
        0..60 => return "just now".to_owned(),
        60..3_600 => (seconds / 60, "m"),
        3_600..86_400 => (seconds / 3_600, "h"),
        86_400..604_800 => (seconds / 86_400, "d"),
        604_800..2_592_000 => (seconds / 604_800, "w"),
        2_592_000..31_536_000 => (seconds / 2_592_000, "mo"),
        _ => (seconds / 31_536_000, "y"),
    };
    format!("{amount}{unit} ago")
}

#[cfg(test)]
mod pictures;
#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::text::Line;
    use time::Duration;
    use time::macros::datetime;

    use super::*;
    use crate::bottom::BottomState;
    use crate::bridge::transcript::picture_check::assert_rows_match;
    use crate::render::{Clock, RenderCtx};
    use crate::theme::{Glyphs, Theme};

    const WIDTH: u16 = 118;
    const NOW: OffsetDateTime = datetime!(2026-10-20 15:00 UTC);

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn row(number: u64, title: &str, labels: &[&str], ago: Duration, yours: bool) -> IssueRow {
        IssueRow {
            number,
            title: title.to_owned(),
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            updated: NOW - ago,
            yours,
        }
    }

    /// The picture's example: a repository's six open issues, two of them yours.
    fn example() -> Vec<IssueRow> {
        vec![
            row(
                42,
                "Rate limiter shares one bucket across API keys",
                &["bug"],
                Duration::hours(5),
                true,
            ),
            row(
                45,
                "Retry-After is sent in milliseconds",
                &["bug"],
                Duration::days(1),
                true,
            ),
            row(
                51,
                "Document the X-Api-Key header",
                &["docs"],
                Duration::hours(3),
                false,
            ),
            row(
                48,
                "429 responses have no JSON body",
                &["enhancement"],
                Duration::days(2),
                false,
            ),
            row(
                39,
                "Refills can overshoot the bucket's capacity",
                &["bug", "good first issue"],
                Duration::days(6),
                false,
            ),
            row(
                31,
                "Let the limiter keep its buckets in Redis",
                &["enhancement", "help wanted"],
                Duration::weeks(3),
                false,
            ),
        ]
    }

    fn drawn(picker: &Picker) -> Vec<Line<'static>> {
        let theme = theme();
        let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
        let mut bottom = BottomState::new();
        bottom.picker = Some(picker.clone());
        bottom.render(&ctx).lines
    }

    fn picture() -> (String, String) {
        let txt = include_str!("../../tests/golden/issue_list.txt");
        let html = include_str!("../../tests/golden/issue_list.html");
        let rows: Vec<&str> = txt.lines().map(str::trim_end).collect();
        (
            rows.join("\n"),
            html.replace("color:#b1b9f9", "color:var(--term-acc)"),
        )
    }

    #[test]
    fn a_cut_list_says_how_many_are_open() {
        assert_eq!(
            cut_about(32, 40),
            "32 of 40 open issues, yours first · /issue <number> starts any"
        );
    }

    #[test]
    fn the_list_takes_the_inputs_place_yours_first() {
        let mut picker = picker("test-user/rate-limiter-app-council", &example(), None, NOW);
        picker.vim = true;
        let (txt, html) = picture();
        // The picture's rule is row 6; the rows above it are the conversation's.
        assert_rows_match(
            "issue_list",
            &drawn(&picker),
            (&txt, &html),
            5..19,
            &theme(),
            &[],
        );
    }

    #[test]
    fn enter_starts_on_the_issue_under_the_pointer_and_a_filter_finds_it_by_number() {
        let mut picker = picker("team/app", &example(), None, NOW);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            PickerEvent::Moved
        );
        let picked = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(picked, PickerEvent::Picked(1));
        assert_eq!(number_of(&picker.rows[1]), Some(45));
        for c in "#39".chars() {
            picker.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(picker.shown(), [4]);
    }

    #[test]
    fn a_problem_or_an_empty_list_says_so_in_place_of_the_rows() {
        let problem = "gh isn't signed in. Sign in with gh auth login, then try again.";
        let picker = super::picker("", &[], Some(problem), NOW);
        let text: Vec<String> = drawn(&picker)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert_eq!(text[1].trim(), "Issues");
        assert!(text.iter().any(|line| line.trim() == problem));
        let empty = super::picker("team/app", &[], None, NOW);
        assert_eq!(empty.placeholder, "No open issues in team/app.");
        // One group alone needs no heading.
        let mine = super::picker("team/app", &example()[..2], None, NOW);
        assert!(mine.rows.iter().all(|row| row.heading.is_none()));
    }

    #[test]
    fn ages_read_as_the_list_writes_them() {
        assert_eq!(ago(NOW - Duration::seconds(5), NOW), "just now");
        assert_eq!(ago(NOW - Duration::minutes(12), NOW), "12m ago");
        assert_eq!(ago(NOW - Duration::days(9), NOW), "1w ago");
        assert_eq!(ago(NOW - Duration::days(40), NOW), "1mo ago");
    }
}
