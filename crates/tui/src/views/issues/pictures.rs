//! `/issue`'s screens drawn by the printers that draw them live, from their pictures' example
//! numbers, and compared cell for cell with `tests/golden/<name>.txt` and its colours at the
//! pictures' width of 118 columns: what `/issue 42` read and left out, the main agent's plan,
//! and the question the plan waits on.

use std::time::Instant;

use argonauts_core::app::{AppChoice, AppEvent, ChoiceKind};
use argonauts_core::base::names::{ChoiceId, ModelRef};
use argonauts_core::runs::events::approval::{AnswerAction, AnswerOption, FreeText};
use ratatui::text::Line;

use crate::app::compose_live;
use crate::app::model::{BottomLine, CliUse, LinePart, ViewModel, WindowUse};
use crate::app::options::UiOptions;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::theme::{Glyphs, Role, Theme};
use crate::bottom::{ApprovalMode, VimMode};
use crate::bridge::Bridge;
use crate::cells::cell::{AssistantMessage, UserMessage};
use crate::testing::pictures::picture_check::assert_rows_match;

const WIDTH: u16 = 118;

fn picture(name: &str) -> (String, String) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/");
    let read = |ext: &str| std::fs::read_to_string(format!("{dir}{name}.{ext}")).unwrap();
    let txt = read("txt")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    (txt, read("html"))
}

/// As on macOS, where the main agent's answer starts with Claude Code's `⏺`.
fn on_macos() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::MACOS;
    theme
}

/// The screen as the bridge leaves it, drawn in full as the full screen draws a frame.
struct Screen {
    bridge: Bridge,
    model: ViewModel,
    now: Instant,
    theme: Theme,
}

impl Screen {
    fn new(theme: Theme) -> Self {
        Self {
            bridge: Bridge::new(&UiOptions::default()),
            model: ViewModel::default(),
            now: Instant::now(),
            theme,
        }
    }

    fn send(&mut self, event: AppEvent) {
        self.bridge.on_event(&mut self.model, &event, self.now);
    }

    fn printed(&mut self) -> Vec<Line<'static>> {
        let ctx = RenderCtx::new(WIDTH, &self.theme, Clock::still(self.now));
        let mut rows = self.model.history.drain_printable(&ctx);
        if let Some(at) = self.model.history.take_started_over() {
            rows.drain(..at);
        }
        rows
    }

    /// `rows` of screen: what printed and the live blocks under it, then the live region
    /// pinned to the bottom.
    fn frame(&mut self, printed: Vec<Line<'static>>, rows: usize) -> Vec<Line<'static>> {
        as_pictured(&mut self.model);
        let ctx = RenderCtx::new(WIDTH, &self.theme, Clock::new(self.now, self.now))
            .with_rows(rows as u16);
        let blocks = self.model.history.live_blocks(&ctx);
        let view = compose_live(
            &blocks,
            &self.model.history,
            &self.model.bottom,
            &ctx,
            rows as u16 - 2,
        );
        let (trailing, pinned) = view.lines.split_at(view.trailing);
        let room = rows.saturating_sub(pinned.len());
        let mut screen = printed;
        screen.extend(trailing.iter().cloned());
        let screen_len = screen.len();
        let mut screen = screen.split_off(screen_len.saturating_sub(room));
        screen.resize(room, Line::default());
        screen.extend(pinned.iter().cloned());
        screen
    }
}

/// The line under the input as the pictures draw it: Vim's INSERT, auto mode, and the three
/// CLIs' plan usage.
fn as_pictured(model: &mut ViewModel) {
    let bottom = &mut model.bottom;
    bottom.composer.label = Some("Opus 5.5".to_owned());
    bottom.composer.examples_off = true;
    bottom.mode.vim = Some(VimMode::Insert);
    bottom.mode.approval = ApprovalMode::Auto;
    bottom.mode.footer_hints = false;
    let cli = |name: &str, windows: &[(&str, f64)]| {
        let mut cli = CliUse::new(name, Role::for_cli(name));
        cli.windows = windows
            .iter()
            .map(|(label, used)| WindowUse::new(*label, *used))
            .collect();
        cli
    };
    let mut claude = cli("Claude", &[("5h", 0.02), ("7d", 0.87)]);
    claude.main = true;
    bottom.line = BottomLine {
        right: vec![LinePart::PlanUsage(vec![
            claude,
            cli("Codex", &[("7d", 0.03)]),
            cli("agy", &[("7d", 0.0)]),
        ])],
        ..BottomLine::default()
    };
}

fn welcome() -> AppEvent {
    AppEvent::Welcome {
        version: "0.1.0".to_owned(),
        repo: "~/workspace/rate-limiter-app-council".to_owned(),
        workflow: "council".to_owned(),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
        main_agent: Some(ModelRef::new(
            "claude-code",
            Some("claude-opus-5-5".to_owned()),
        )),
        effort: Some("xhigh".to_owned()),
    }
}

const PLAN: &str = "Here is #42 in my words: all requests share one token bucket, so one busy \
API key slows every other key. The plan:

1. Give each X-Api-Key its own bucket in ratelimit/bucket.py, made on the key's first request.
2. Requests with no key share one anonymous bucket, as the maintainer's comment on the issue asks.
3. Tests that two keys don't drain each other's bucket, and that the anonymous bucket still limits.

The crew works on it as a Council, on its own branch, and it ends as a draft pull request that \
you see before anything is sent.";

/// The plan's question as the app asks it.
fn plan_question() -> AppChoice {
    let option = |key: char, label: &str, value: &str, detail: Option<&str>| AnswerOption {
        key,
        label: label.to_owned(),
        action: AnswerAction::Choose {
            value: value.to_owned(),
        },
        recommended: false,
        detail: detail.map(str::to_owned),
    };
    AppChoice {
        id: ChoiceId::numbered(1),
        kind: ChoiceKind::GoAhead,
        title: "Start the crew on this plan?".to_owned(),
        body: vec![
            "The crew gets this plan. The issue's text goes with it as a description of the \
             task, never as instructions."
                .to_owned(),
        ],
        options: vec![
            option('y', "Yes, start", "issue:start", None),
            option(
                'c',
                "Change the plan",
                "issue:change",
                Some("say what to change, and the main agent writes it again"),
            ),
            option(
                'n',
                "No",
                "issue:no",
                Some("nothing starts, and nothing is posted on the issue"),
            ),
        ],
        free_text: FreeText::Guidance,
        bell: true,
    }
}

#[test]
fn an_issue_read_back_and_waiting_on_its_plan_is_its_picture() {
    let mut screen = Screen::new(on_macos());
    screen.send(welcome());
    screen.printed();
    screen.model.history.push(UserMessage::command("/issue 42"));
    screen.send(AppEvent::Report {
        title: "Issue #42 · Rate limiter shares one bucket across API keys".to_owned(),
        lines: vec![
            "Read: the title, the body, 3 comments by people with write access".to_owned(),
            "Left out: 2 comments by others (/issue 42 --all-comments reads them) · hidden text \
             removed"
                .to_owned(),
            "Works on branch issue-42-per-key-limiter".to_owned(),
        ],
    });
    screen.model.history.push(AssistantMessage::new(PLAN));
    screen.send(AppEvent::Choice {
        choice: plan_question(),
    });
    let printed = screen.printed();
    // Tall enough for the two empty rows the picture keeps over the question.
    let frame = screen.frame(printed, 29);
    let echo = frame
        .iter()
        .position(|line| line.to_string().starts_with("❯ /issue 42"))
        .expect("the command's row");
    let (txt, html) = picture("issue_start");
    // The line under the input is left out: while a question holds the keys, the input's Vim
    // mode isn't shown, where the picture keeps `-- INSERT --` from the pictures it starts from.
    assert_rows_match(
        "issue_start",
        &frame[echo..frame.len() - 1],
        (&txt, &html),
        2..29,
        &screen.theme,
    );
}

/// The draft question's words as the run writes them, from the picture's example.
const DRAFT: &str = "branch issue-42-per-key-limiter · 2 files +30 \u{2212}4 · checks 5/5 · Codex approved, agy signed off
It posts as you (gh: test-user) · nothing is posted on the issue itself

title   Give each API key its own rate limit bucket
body    Gives each X-Api-Key its own token bucket, so one busy key no longer slows the others.
        Requests with no key share one anonymous bucket.
        … 6 more lines: the checks, who reviewed, and how it was made
        Fixes #42";

/// The draft question as the run opens it.
fn draft_question() -> argonauts_core::runs::events::approval::ApprovalOpened {
    use argonauts_core::base::names::ApprovalId;
    use argonauts_core::runs::events::approval::{
        ApprovalKind, ApprovalOpened, ApprovalPacket, QuestionPacket,
    };
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
    use argonauts_core::settings::asking::WhenDeclined;
    let option = |key: char, label: &str, value: &str, detail: &str| AnswerOption {
        key,
        label: label.to_owned(),
        action: AnswerAction::Choose {
            value: value.to_owned(),
        },
        recommended: false,
        detail: Some(detail.to_owned()),
    };
    ApprovalOpened {
        id: ApprovalId::numbered(1),
        kind: ApprovalKind::Question,
        title: "Open a draft pull request on GitHub?".to_owned(),
        task: None,
        packet: ApprovalPacket::Question(QuestionPacket {
            from: ModelRef::new("claude-code", Some("claude-opus-5-5".to_owned())),
            task: argonauts_core::TaskId::new("task-001").unwrap(),
            text: DRAFT.to_owned(),
            options: Vec::new(),
        }),
        evidence_sha256: "0".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        when_declined: WhenDeclined::Decide,
        options: vec![
            option(
                'p',
                "Push and open the draft",
                "issue:push",
                "pushes the branch to origin, then opens it as a draft",
            ),
            option(
                'b',
                "Show the whole body",
                "issue:body",
                "the body in full, then these choices again",
            ),
            option(
                'k',
                "Keep the branch here, post nothing",
                "issue:keep",
                "the run ends with the branch kept",
            ),
            option(
                'a',
                "Allow GitHub writes for the rest of this run",
                "issue:allow",
                "this and any later comment or reply in the run go without asking",
            ),
        ],
        free_text: FreeText::Guidance,
        needs_you: true,
    }
}

#[test]
fn the_draft_a_run_on_an_issue_ends_with_is_its_picture() {
    let theme = on_macos();
    let mut question = crate::questions::approval_question(1, &draft_question(), None);
    question.focused = true;
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let lines = question.lines(&ctx);
    let (txt, html) = picture("issue_draft");
    assert_rows_match("issue_draft", &lines, (&txt, &html), 2..19, &theme);
}

/// The draft pull request #57 whose check failed, followed in a Pull request phase after the
/// council's own, and its row in the bottom list as it goes on.
mod pr_follow {
    use std::time::Instant;

    use argonauts_core::app::views::{
        AgentRow, PhaseCount, PhaseRow, RowNote, RowStatus, StatusKind, StatusLine, Tone,
        WorkflowRow, WorkflowState, WorkflowView as WorkflowData,
    };
    use argonauts_core::base::names::{SessionKey, WorkflowId};
    use argonauts_core::workflows::workflow::StandardPhase;
    use argonauts_core::{RunId, Spend};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use crate::frame::view::{View, ViewCtx};
    use crate::testing::pictures::workflow_pictures::{Picture, dark};
    use crate::views::workflow::WorkflowView;

    /// The frame stops six cells short of the screen's right edge, as Claude Code's does.
    const SCREEN: u16 = 124;
    const HAIKU: &str = "claude-code/claude-haiku-5-5";
    const LUNA: &str = "codex/gpt-6-luna";
    const NAME: &str = "Per key limiter";

    fn agent(session: u32, label: &str, model: &str, status: RowStatus) -> AgentRow {
        AgentRow {
            session: SessionKey::numbered(session),
            label: label.to_owned(),
            worker: model.parse().expect("a model"),
            status,
            tokens: 0,
            spent: Spend::default(),
            elapsed_ms: 0,
            subagents: 0,
            context: None,
            fallback_for: None,
            answered: false,
            tries: Vec::new(),
            note: None,
            inner: None,
            left_out: false,
            not_needed: false,
            fallback_why: None,
        }
    }

    fn done_phase(name: &str, agents: u32, standard: StandardPhase) -> PhaseRow {
        PhaseRow {
            name: name.to_owned(),
            detail: String::new(),
            standard,
            status: RowStatus::Done,
            try_number: None,
            count: PhaseCount::of(agents, agents),
            rows: Vec::new(),
            note: None,
        }
    }

    /// The picture's example: the council done, and round 1 of the pull request's follow-up
    /// with its fixer at work and its reviewer queued.
    fn following() -> WorkflowData {
        let mut fixer = agent(12, "fix:ci-round-1", HAIKU, RowStatus::Running);
        fixer.tokens = 6_300;
        fixer.elapsed_ms = 38_000;
        fixer.note = Some(RowNote::new(
            "the builder, resumed, with the log",
            Tone::Plain,
        ));
        let reviewer = agent(13, "review:ci-round-1", LUNA, RowStatus::Queued);
        let pull_request = PhaseRow {
            name: "Pull request".to_owned(),
            detail: String::new(),
            standard: StandardPhase::Check,
            status: RowStatus::Running,
            try_number: None,
            count: PhaseCount::of(0, 2),
            rows: vec![WorkflowRow::Agent(fixer), WorkflowRow::Agent(reviewer)],
            note: None,
        };
        WorkflowData {
            run: RunId::new("run-01").expect("a run id"),
            id: WorkflowId::numbered(1),
            name: NAME.to_owned(),
            description: "issue #42 · Give each X-Api-Key its own token bucket, and one shared \
                          bucket for requests with no key."
                .to_owned(),
            style: None,
            status_lines: vec![StatusLine::new(
                StatusKind::PullRequest,
                "#57 · test_rate_limit failed on Ubuntu · back to its builder, round 1 of 3 · \
                 the fix is pushed once reviewed",
            )],
            agents_done: 11,
            agents_total: 13,
            elapsed_ms: 846_000,
            spent: Spend::default(),
            phases: vec![
                done_phase("Propose", 3, StandardPhase::Do),
                done_phase("Challenge", 3, StandardPhase::Do),
                done_phase("Decide", 1, StandardPhase::Do),
                done_phase("Build", 1, StandardPhase::Do),
                done_phase("Cross-review", 1, StandardPhase::Check),
                done_phase("Sign-off", 1, StandardPhase::Check),
                pull_request,
            ],
            notes: vec!["Council".to_owned()],
            required: Vec::new(),
            goals: Vec::new(),
            worker: None,
            state: WorkflowState::Running,
        }
    }

    fn drawn(view: &mut WorkflowView, rows: u16) -> Buffer {
        let theme = dark();
        let state = crate::views::workflow::tests::nested::state();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        let area = Rect::new(0, 0, SCREEN, rows);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        buf
    }

    fn select_last(view: &mut WorkflowView) {
        let theme = dark();
        let state = crate::views::workflow::tests::nested::state();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: Instant::now(),
        };
        for _ in 0..following().phases.len() {
            view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &ctx);
        }
    }

    fn the_picture() -> Picture {
        crate::testing::pictures::workflow_pictures::picture!("pr_follow")
    }

    /// The frame is drawn in the text's colour, as the approved council picture draws it,
    /// where this picture draws it faint.
    fn frame_as_drawn(picture: &mut Picture) {
        for row in 6..=14 {
            let text = picture.text(row..row + 1).remove(0);
            for (column, ch) in text.chars().enumerate() {
                if "╭╮╰╯│─┬┴".contains(ch) {
                    picture.recolour(row, column, None);
                }
            }
        }
    }

    #[test]
    fn a_followed_pull_request_with_a_failed_check_reads_as_its_picture() {
        let mut picture = the_picture();
        frame_as_drawn(&mut picture);
        let mut view = WorkflowView::new(following());
        select_last(&mut view);
        let buf = drawn(&mut view, 15);
        // The header's third line says where the pull request stands: `◐`, amber up to the
        // failure and dim after it.
        picture.assert_drawn(1..5, &buf, 0);
        picture.assert_drawn(5..16, &buf, 4);
    }
}
