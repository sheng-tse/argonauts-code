//! A resumed conversation, drawn by the printers that drew it live, from the council session's
//! own records in `golden/resumed_council/` (its home folder made neutral, its plan readings and
//! its CLI's lists left out), compared cell for cell with `golden/resumed_after.txt` at its width
//! of 118 columns.

use std::time::Instant;

use argonauts_core::app::{AppEvent, ConversationEvent};
use argonauts_core::events::run::{RunLifecycle, RunOutcome};
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::names::{ConversationId, ModelRef, TurnId};
use ratatui::text::Line;
use time::macros::datetime;

use super::Bridge;
use super::transcript::picture_check::{Unlike, assert_matches, assert_rows_match};
use crate::format::WallClock;
use crate::model::ViewModel;
use crate::options::UiOptions;
use crate::render::{Clock, RenderCtx};
use crate::text::plain_text;
use crate::theme::{Glyphs, Role, Theme};

const CONVERSATION: &str = include_str!("../../golden/resumed_council/conversation-001.jsonl");
const RUN: &str = include_str!("../../golden/resumed_council/events.jsonl");
const PICTURE: &str = include_str!("../../golden/resumed_after.txt");
const COLOURS: &str = include_str!("../../golden/resumed_after.html");
const WIDTH: u16 = 118;

/// The picture's rows up to and with the main agent's answer, which this test holds the screen
/// to cell for cell. What the run printed after it is checked block by block below.
const OWN_ROWS: usize = 11;

/// The start header's ship keeps the app's own orange, where the picture paints it amber.
const LOGO: &str = "the logo is in the app's own orange";

/// As on macOS, where items start with Claude Code's `⏺`, as the picture draws them.
fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::MACOS;
    theme
}

struct Screen {
    bridge: Bridge,
    model: ViewModel,
}

impl Screen {
    /// The screen as it was at 12:35 AM on October 15, in the zone the run was recorded in.
    fn new() -> Self {
        let mut bridge = Bridge::new(&UiOptions::default());
        bridge.transcript = super::transcript::Transcript::with_clock(WallClock::pinned(
            datetime!(2026-10-15 00:35 -4),
        ));
        Self {
            bridge,
            model: ViewModel::default(),
        }
    }

    fn send(&mut self, event: AppEvent) {
        self.bridge
            .on_event(&mut self.model, &event, Instant::now());
    }

    fn printed(&mut self) -> Vec<Line<'static>> {
        let theme = theme();
        let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
        self.model.history.drain_printable(&ctx)
    }
}

/// The names the CLIs gave their models that day, `haiku` among Claude Code's aliases.
fn names() -> AppEvent {
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::model_catalog::CatalogEntry;
    let mut names = DisplayNames::default();
    for (cli, model, name, aliases) in [
        (
            "claude-code",
            "claude-haiku-5-5",
            "Haiku 5.5",
            vec!["haiku".to_owned()],
        ),
        ("codex", "gpt-6-luna", "GPT 6 Luna", Vec::new()),
        (
            "antigravity-cli",
            "gemini-3.6-flash-low",
            "Gemini 3.6 Flash (Low)",
            Vec::new(),
        ),
    ] {
        let mut entry = CatalogEntry::new(ModelRef::new(cli, Some(model.to_owned())));
        entry.name = Some(name.to_owned());
        entry.aliases = Some(aliases).filter(|aliases| !aliases.is_empty());
        names.catalog.models.push(entry);
    }
    for (cli, name) in [
        ("claude-code", "Claude Code"),
        ("codex", "Codex"),
        ("antigravity-cli", "Antigravity CLI"),
        ("opencode", "OpenCode"),
    ] {
        names.clis.insert(cli.to_owned(), name.to_owned());
    }
    AppEvent::Names {
        names: Box::new(names),
    }
}

fn welcome() -> AppEvent {
    AppEvent::Welcome {
        version: "0.0.1".to_owned(),
        repo: "~/workspace/rate-limiter-app/council".to_owned(),
        workflow: "council".to_owned(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        main_agent: Some(ModelRef::new(
            "claude-code",
            Some("claude-haiku-5-5".into()),
        )),
        effort: Some("low".to_owned()),
    }
}

/// The run's record, each line an event.
fn record() -> Vec<RunEvent> {
    RUN.lines()
        .map(|line| serde_json::from_str(line).expect("a run's journal line"))
        .collect()
}

fn run(event: RunEvent) -> AppEvent {
    AppEvent::Run {
        event: Box::new(event),
    }
}

/// The conversation as the controller sends it back: its record in order, and the run its first
/// turn started right after that turn, all between the past's two marks.
fn resumed(screen: &mut Screen) {
    resumed_with(screen, record());
}

/// [`resumed`], with `events` for the run's record.
fn resumed_with(screen: &mut Screen, events: Vec<RunEvent>) {
    screen.send(AppEvent::ConversationResumed {
        conversation: ConversationId::numbered(1),
        title: "Make the rate limiter per API key".to_owned(),
        worker: Some(ModelRef::new(
            "claude-code",
            Some("claude-haiku-5-5".into()),
        )),
        turns: Vec::new(),
        artifacts: Vec::new(),
    });
    screen.send(AppEvent::PastBegins);
    for line in CONVERSATION.lines() {
        let event: ConversationEvent = serde_json::from_str(line).expect("a journal line");
        let ended = matches!(&event, ConversationEvent::TurnFinished { turn, .. }
            if *turn == TurnId::numbered(1));
        screen.send(AppEvent::Conversation { event });
        if ended {
            for event in events.clone() {
                screen.send(run(event));
            }
        }
    }
    screen.send(AppEvent::PastDrawn);
}

/// The picture's rows after the main agent's answer, a run of them at a time, each found in
/// the drawing by its first two rows. Between them the screen draws what the picture leaves
/// out, each block of it named in [`LEFT_OUT`].
const PARTS: [std::ops::Range<usize>; 6] = [12..15, 15..26, 26..28, 28..30, 30..35, 38..40];

/// What the screen prints that the picture leaves out, by each block's first row, all of it as
/// it printed live: the main agent's turn ending, the run's start note, and the two fallbacks Argonauts refused to start for want of a model, which runs today
/// never pick. The picture's rows 35 to 37 stand for slash commands typed between two tries,
/// which this record doesn't hold.
const LEFT_OUT: [&str; 4] = [
    "✻ Sailed for 10s",
    "⏺ As you asked: For the main agent: try spec-first",
    "✗ propose:p3 · OpenCode · 0s",
    "✗ propose:p3 · opencode-acp · 0s",
];

/// The planner's calls print in its one row, between its header and its Done line. The picture
/// leaves them out and draws the Done line right under the header, as a row's first line with
/// its `⎿`. The drawing as the picture has it, and the calls it leaves out.
fn without_the_planners_calls(printed: Vec<Line<'static>>) -> (Vec<Line<'static>>, String) {
    let text = |line: &Line<'_>| line.to_string();
    let header = printed
        .iter()
        .position(|line| text(line).starts_with("⏺ plan:orchestrator · "))
        .expect("the planner's row");
    let done = (header + 1..printed.len())
        .find(|&at| text(&printed[at]).trim_start().starts_with("Done · "))
        .expect("the planner's Done line");
    let calls: Vec<String> = printed[header + 1..done].iter().map(text).collect();
    let mut lines = printed;
    let done_line = lines[done].clone();
    let mut spans = vec![ratatui::text::Span::styled(
        "  ⎿  ",
        theme().style(Role::Dim),
    )];
    let mut indent = 5;
    for span in done_line.spans {
        let content = span.content.to_string();
        let kept: String = content
            .chars()
            .skip_while(|ch| {
                let skip = indent > 0 && *ch == ' ';
                if skip {
                    indent -= 1;
                }
                skip
            })
            .collect();
        if !kept.is_empty() {
            spans.push(ratatui::text::Span::styled(kept, span.style));
        }
    }
    lines[done] = Line::from(spans).style(done_line.style);
    lines.drain(header + 1..done);
    (lines, calls.join("\n"))
}

/// A failed agent's row starts with the app's own `✗`, where the picture draws `✘`.
const CROSS: &str = "a failed agent's row is marked ✗, as every failed agent's is";

/// A Workflow call that returned has its `⏺` green, as Claude Code's own calls do, where the
/// picture paints it plain.
const RETURNED: &str = "a Workflow call that returned has its bullet green";

/// An agent's model reads in its shade, as the council's rows draw it, where this picture
/// draws it dim.
const MODEL: &str = "an agent's model reads in its shade, as on every agent's row";

/// A fallback that skips its retry names who steps in plain, its bullet warning, where the
/// picture paints the words amber.
const NO_RETRY: &str =
    "a fallback that skips its retry names who steps in plain, as every such note does";

/// The `❯` on your message's band is faint, as on every message of yours.
const YOUR_MARK: &str = "the ❯ on your message's band is faint, as on every message of yours";

/// The cells of the picture's `row` the screen draws otherwise, as row `at` of its part.
fn unlike(row: &str, at: usize) -> Vec<Unlike> {
    let mut unlike = Vec::new();
    if let Some((head, _)) = row.split_once(" · no second try") {
        unlike.push(Unlike {
            row: at,
            cols: (2, head.chars().count() - 1),
            why: NO_RETRY,
        });
        return unlike;
    }
    if row.starts_with("⏺ Workflow(") {
        unlike.push(Unlike {
            row: at,
            cols: (0, 0),
            why: RETURNED,
        });
        return unlike;
    }
    if row.starts_with('✘') {
        unlike.push(Unlike {
            row: at,
            cols: (0, 0),
            why: CROSS,
        });
    }
    let header = row
        .get(row.char_indices().nth(2).map_or(0, |(i, _)| i)..)
        .and_then(|rest| rest.split_once(" · "))
        .filter(|(label, _)| label.contains(':') && !label.contains(' '));
    if let Some((label, who)) = header
        && let Some((model, _)) = who.split_once(" on ")
    {
        let start = 2 + label.chars().count() + " · ".chars().count();
        unlike.push(Unlike {
            row: at,
            cols: (start, start + model.chars().count() - 1),
            why: MODEL,
        });
    }
    unlike
}

/// From the start header to the run's last try the drawing is the picture's: the Workflow line
/// by the name the council's script reported, before the planner that wrote it; each agent by
/// the model its command launched; the wait said once, with one line per try, as the record
/// doesn't say what woke each.
#[test]
fn a_resumed_conversation_is_drawn_as_it_printed_live() {
    let mut screen = Screen::new();
    screen.send(names());
    screen.send(welcome());
    resumed(&mut screen);
    let (printed, calls) = without_the_planners_calls(screen.printed());
    assert!(calls.starts_with("  ⎿  Search("), "{calls}");
    crate::picture_dump::lines("resumed_after", &printed);
    for line in &printed {
        assert!(
            crate::text::str_width(&line.to_string()) <= usize::from(WIDTH),
            "{line}"
        );
    }
    // The picture pads each row to its width; the screen leaves the rest of a row empty.
    let want: Vec<&str> = PICTURE.lines().map(str::trim_end).collect();
    assert_matches(
        "resumed_after_transcript",
        &printed[..OWN_ROWS],
        &want[..OWN_ROWS].join("\n"),
        COLOURS,
        &theme(),
        &[
            Unlike {
                row: 1,
                cols: (0, 9),
                why: LOGO,
            },
            Unlike {
                row: 2,
                cols: (0, 9),
                why: LOGO,
            },
            Unlike {
                row: 3,
                cols: (0, 9),
                why: LOGO,
            },
            Unlike {
                row: 6,
                cols: (0, 0),
                why: YOUR_MARK,
            },
        ],
    );
    let rows: Vec<String> = printed
        .iter()
        .map(|line| line.to_string().trim_end().to_owned())
        .collect();
    let picture = want.join("\n");
    let mut covered = vec![false; rows.len()];
    covered[..=OWN_ROWS].fill(true);
    let mut after = OWN_ROWS;
    for part in PARTS {
        let first = &want[part.start..part.start + 2];
        let at = (after..rows.len().saturating_sub(1))
            .find(|&at| rows[at] == first[0] && rows[at + 1] == first[1])
            .unwrap_or_else(|| {
                panic!(
                    "rows {part:?} of the picture aren't drawn:\n{}",
                    rows.join("\n")
                )
            });
        let unlike: Vec<Unlike> = (part.start..part.end)
            .flat_map(|row| unlike(want[row], row - part.start))
            .collect();
        let end = at + part.len();
        assert_rows_match(
            "resumed_after",
            &printed[at..end],
            (&picture, COLOURS),
            part,
            &theme(),
            &unlike,
        );
        covered[at..end].fill(true);
        after = end;
    }
    assert_eq!(after, rows.len(), "nothing prints after the last try");
    // Every row the picture leaves out belongs to a block named for it.
    let mut left_out = Vec::new();
    let mut at = 0;
    while at < rows.len() {
        if covered[at] || rows[at].is_empty() {
            at += 1;
            continue;
        }
        let block: Vec<&str> = rows[at..]
            .iter()
            .take_while(|row| !row.is_empty())
            .map(String::as_str)
            .collect();
        at += block.len();
        left_out.push(block.join("\n"));
    }
    assert_eq!(left_out.len(), LEFT_OUT.len(), "{left_out:#?}");
    for (block, named) in left_out.iter().zip(LEFT_OUT) {
        assert!(block.starts_with(named), "{block}\nisn't {named}");
    }
    let text = plain_text(&printed);
    for summed_up in [
        "So far in this run",
        "※ recap",
        "ctrl+o shows the whole history",
        "An agent it waited for came back",
        "It tries again on its own",
    ] {
        assert!(!text.contains(summed_up), "{summed_up}: {text}");
    }
    assert_eq!(
        text.matches("❯ Make the rate limiter per API key").count(),
        1,
        "your words print once, as you typed them"
    );
}

/// A try that woke from the wait and ended otherwise prints in full, saying only when it began,
/// since the record doesn't say what woke it.
#[test]
fn a_try_that_ends_otherwise_prints_in_full() {
    let mut events: Vec<RunEvent> = record().into_iter().take_while(|e| e.seq <= 151).collect();
    let mut finished = events.last().expect("an event").clone();
    finished.seq += 1;
    finished.kind = RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Stopped,
        detail: None,
    });
    events.push(finished);
    let mut screen = Screen::new();
    screen.send(names());
    screen.send(welcome());
    resumed_with(&mut screen, events);
    let text = plain_text(&screen.printed());
    assert_eq!(text.matches("✢ The run waits").count(), 1, "{text}");
    assert!(!text.contains("✢ Tried again at"), "{text}");
    assert!(
        text.contains(
            "⏺ Resume(Make the rate limiter per API key: each X-Api-Key gets its…)\n  ⎿  Tried \
             again at 1:59 AM"
        ),
        "{text}"
    );
    assert!(!text.contains("An agent it waited for came back"), "{text}");
}

/// Live, the agent that writes a script's workflow prints once the Workflow line has, since
/// nothing of a workflow draws before its shape is known.
#[test]
fn the_planner_prints_after_the_workflow_line_it_led_to() {
    let mut screen = Screen::new();
    screen.send(names());
    screen.send(welcome());
    let events = record();
    let (before, after): (Vec<RunEvent>, Vec<RunEvent>) =
        events.into_iter().partition(|event| event.seq <= 86);
    for event in before {
        screen.send(run(event));
    }
    let text = plain_text(&screen.printed());
    assert!(!text.contains("plan:orchestrator"), "{text}");
    for event in after.into_iter().take(1) {
        screen.send(run(event));
    }
    let text = plain_text(&screen.printed());
    let workflow = text.find("⏺ Workflow(Per key rate limit · Council)");
    let planner = text.find("⏺ plan:orchestrator · Haiku 5.5 on Claude Code");
    assert!(
        workflow.is_some() && planner.is_some() && workflow < planner,
        "{text}"
    );
}

/// A run that ends before its shape is known still prints the agents that ended.
#[test]
fn a_run_that_ends_unshaped_prints_its_planner() {
    let mut screen = Screen::new();
    screen.send(names());
    screen.send(welcome());
    let mut events: Vec<RunEvent> = record().into_iter().take_while(|e| e.seq <= 86).collect();
    let mut finished = events.last().expect("an event").clone();
    finished.seq += 1;
    finished.session = None;
    finished.kind = RunEventKind::Run(RunLifecycle::Finished {
        outcome: RunOutcome::Failed,
        detail: None,
    });
    events.push(finished);
    for event in events {
        screen.send(run(event));
    }
    let text = plain_text(&screen.printed());
    assert!(
        planner_block(&text).contains("\n     Done · 5 tool uses"),
        "{text}"
    );
}

/// A run that comes on screen after it started elsewhere draws its past first, then attaches:
/// an agent still at work keeps the block its past drew, every call in it, so its end counts
/// them all.
#[test]
fn an_agent_at_work_keeps_the_block_its_past_drew_once_the_run_attaches() {
    use argonauts_core::Spend;
    use argonauts_core::api::{RunState as RunStatus, RunSummary};
    use argonauts_core::events::run::RunMode;
    let mut screen = Screen::new();
    screen.send(names());
    screen.send(welcome());
    let (before, after): (Vec<RunEvent>, Vec<RunEvent>) =
        record().into_iter().partition(|event| event.seq <= 50);
    let run_id = before[0].run_id.clone();
    screen.send(AppEvent::PastBegins);
    for event in before {
        screen.send(run(event));
    }
    screen.send(AppEvent::PastDrawn);
    screen.send(AppEvent::Attached {
        summary: RunSummary {
            run: run_id.clone(),
            title: "Make the rate limiter per API key".into(),
            mode: RunMode::Crew,
            workflow: "council".into(),
            state: RunStatus::Running,
            stopped_at: "working".into(),
            updated: datetime!(2026-10-14 05:53 UTC),
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: vec![],
            live: true,
            remind: false,
            name: None,
            shape_known: true,
        },
        started_here: false,
    });
    screen.send(AppEvent::ReplayDone {
        run: run_id,
        seq: 50,
    });
    for event in after.into_iter().take_while(|event| event.seq <= 90) {
        screen.send(run(event));
    }
    let text = plain_text(&screen.printed());
    let block = planner_block(&text);
    assert!(
        block.ends_with("     Done · 5 tool uses · report ✓ · 31s · 34.6k tokens"),
        "{text}"
    );
    assert_eq!(
        block.matches("Read(").count(),
        3,
        "every call is in it: {block}"
    );
}

/// The planner's one row: its header, its calls and its Done line.
fn planner_block(text: &str) -> &str {
    let header = "⏺ plan:orchestrator · Haiku 5.5 on Claude Code";
    let at = text
        .find(header)
        .unwrap_or_else(|| panic!("no planner row: {text}"));
    let block = &text[at..];
    block.split("\n\n").next().unwrap_or(block)
}

/// What each later try printed stays for the transcript overlay, in order, after the wait.
#[test]
fn the_tries_a_wait_folds_stay_for_the_overlay() {
    let mut screen = Screen::new();
    screen.send(names());
    screen.send(welcome());
    resumed(&mut screen);
    let printed = plain_text(&screen.printed());
    let theme = theme();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let kept: Vec<String> = screen
        .model
        .history
        .cells()
        .iter()
        .map(|cell| plain_text(&crate::cell::HistoryCell::lines(cell, &ctx)))
        .collect();
    let resumes = kept
        .iter()
        .filter(|cell| cell.starts_with("⏺ Resume("))
        .count();
    assert_eq!(resumes, 3, "{kept:#?}");
    assert!(!printed.contains("⏺ Resume("), "{printed}");
    let wait = kept
        .iter()
        .position(|cell| cell.starts_with("✢ The run waits"))
        .expect("the wait");
    let first_try = kept
        .iter()
        .position(|cell| cell.starts_with("⏺ Resume("))
        .expect("a try");
    assert!(wait < first_try);
}

/// A command you typed comes back with the past where you typed it, its echo as it printed
/// then. Live, the screen printed it already, and the record's line adds nothing.
#[test]
fn a_typed_command_prints_again_with_the_past() {
    let typed = |text: &str| AppEvent::Conversation {
        event: ConversationEvent::CommandTyped {
            text: text.to_owned(),
            at: None,
        },
    };
    let mut screen = Screen::new();
    screen.send(welcome());
    screen.printed();
    screen.send(AppEvent::PastBegins);
    screen.send(typed("/usage"));
    screen.send(AppEvent::PastDrawn);
    let text = plain_text(&screen.printed());
    assert!(text.contains("\u{276f} /usage"), "{text}");
    screen.send(typed("/model"));
    let text = plain_text(&screen.printed());
    assert!(!text.contains("/model"), "{text}");
}

/// What a command you typed printed back comes back under it, as it printed then: the value
/// after the words, or the whole line dim. Live, the screen printed it already.
#[test]
fn a_typed_commands_answer_prints_again_under_it() {
    use argonauts_core::app::CommandReply;
    let typed = |text: &str| AppEvent::Conversation {
        event: ConversationEvent::CommandTyped {
            text: text.to_owned(),
            at: None,
        },
    };
    let replied = |text: &str, value: Option<&str>, dim: bool| AppEvent::Conversation {
        event: ConversationEvent::CommandReplied {
            reply: CommandReply {
                text: text.to_owned(),
                value: value.map(str::to_owned),
                after: None,
                dim,
                error: false,
            },
        },
    };
    let mut screen = Screen::new();
    screen.send(welcome());
    screen.printed();
    screen.send(AppEvent::PastBegins);
    screen.send(typed("/model"));
    screen.send(replied("Kept model as", Some("Opus 5.5"), false));
    screen.send(typed("/config"));
    screen.send(replied("Set Editor mode to vim", None, true));
    screen.send(AppEvent::PastDrawn);
    let text = plain_text(&screen.printed());
    assert!(
        text.contains("\u{276f} /model\n  ⎿  Kept model as Opus 5.5"),
        "{text}"
    );
    assert!(
        text.contains("\u{276f} /config\n  ⎿  Set Editor mode to vim"),
        "{text}"
    );
    screen.send(replied("Kept theme as", Some("dark"), false));
    let text = plain_text(&screen.printed());
    assert!(!text.contains("Kept theme"), "{text}");
}
