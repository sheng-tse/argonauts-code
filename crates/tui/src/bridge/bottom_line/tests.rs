use argonauts_core::app::{PlanUsageChip, PublishedArtifact};
use argonauts_core::base::names::AgentId;
use argonauts_core::models::limits::PlanUsageSource;
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::bottom_line::BottomLineSettings;
use time::macros::datetime;

use super::*;

const T0: OffsetDateTime = datetime!(2026-09-30 10:00 UTC);

fn clock() -> Clock {
    Clock {
        now: T0 + time::Duration::minutes(1),
        utc_offset: UtcOffset::UTC,
    }
}

fn reading(agent_id: &str, window: &str, used: f64) -> PlanUsageReading {
    PlanUsageReading {
        agent_id: AgentId(agent_id.to_owned()),
        status: PlanUsageStatus::Allowed,
        used: Some(used),
        resets_at: None,
        window: Some(window.to_owned()),
        source: PlanUsageSource::RateLimitEvent,
        observed_at: T0,
        detail: None,
        wrap_up: false,
    }
}

fn cli(label: &str, agent_id: &str, readings: Vec<PlanUsageReading>) -> CrewCli {
    CrewCli {
        label: label.to_owned(),
        agent_id: Some(agent_id.to_owned()),
        health: HealthState::Healthy,
        readings,
    }
}

fn claude() -> CrewCli {
    cli(
        "Claude Code",
        "claude-code",
        vec![
            reading("claude-code", "seven_day", 0.56),
            reading("claude-code", "five_hour", 0.01),
        ],
    )
}

/// A window as a test reads it: its label and the percent used.
type Shown = (String, u8);

fn shown(chip: &CliUse) -> Vec<Shown> {
    chip.windows
        .iter()
        .map(|window| (window.label.clone(), (window.used * 100.0).round() as u8))
        .collect()
}

/// The CLI in use shows both its windows, the shorter first, and the others
/// their fuller one.
#[test]
fn the_cli_in_use_shows_both_windows_and_the_others_their_fuller_one() {
    let set = PlanUsageLine::default();
    let main = cli_use(&claude(), true, &set, None, &clock());
    assert!(main.main);
    assert_eq!(main.color, Role::CliClaude);
    assert_eq!(shown(&main), [("5h".to_owned(), 1), ("7d".to_owned(), 56)]);
    assert!(main.windows.iter().all(|window| window.resets.is_none()));

    let other = cli_use(&claude(), false, &set, None, &clock());
    assert_eq!(shown(&other), [("7d".to_owned(), 56)]);

    let pick = |windows| PlanUsageLine {
        windows,
        ..set.clone()
    };
    let fullest = cli_use(
        &claude(),
        true,
        &pick(PlanUsageWindows::Fullest),
        None,
        &clock(),
    );
    assert_eq!(shown(&fullest), [("7d".to_owned(), 56)]);
    let five = cli_use(
        &claude(),
        false,
        &pick(PlanUsageWindows::FiveHours),
        None,
        &clock(),
    );
    assert_eq!(shown(&five), [("5h".to_owned(), 1)]);
    let seven = cli_use(
        &claude(),
        true,
        &pick(PlanUsageWindows::SevenDays),
        None,
        &clock(),
    );
    assert_eq!(shown(&seven), [("7d".to_owned(), 56)]);
}

/// A reset time shows only from 90%, as Claude Code writes one, and a window near its limit
/// shows whatever the settings pick.
#[test]
fn a_window_near_its_limit_shows_its_reset_time_and_is_never_hidden() {
    let mut near = reading("claude-code", "five_hour", 0.93);
    near.resets_at = Some(datetime!(2026-09-30 16:40 UTC));
    let mut week = reading("claude-code", "seven_day", 0.56);
    week.resets_at = Some(datetime!(2026-10-06 15:45 UTC));
    let claude = cli("Claude Code", "claude-code", vec![week, near]);
    let set = PlanUsageLine::default();

    let chip = cli_use(&claude, true, &set, None, &clock());
    assert!(chip.needs_you());
    assert_eq!(chip.windows[0].resets.as_deref(), Some("4:40pm"));
    assert_eq!(chip.windows[1].resets, None);

    // Not in use, and set to show only the seven days: the window at 93% still shows.
    let seven = PlanUsageLine {
        windows: PlanUsageWindows::SevenDays,
        ..set.clone()
    };
    let chip = cli_use(&claude, false, &seven, None, &clock());
    assert_eq!(shown(&chip), [("5h".to_owned(), 93), ("7d".to_owned(), 56)]);

    let always = PlanUsageLine {
        resets: PlanUsageResets::Always,
        ..set.clone()
    };
    let chip = cli_use(&claude, true, &always, None, &clock());
    assert_eq!(chip.windows[1].resets.as_deref(), Some("Tue 3:45pm"));
    let never = PlanUsageLine {
        resets: PlanUsageResets::Never,
        ..set.clone()
    };
    let chip = cli_use(&claude, true, &never, None, &clock());
    assert_eq!(chip.windows[0].resets, None);
}

#[test]
fn a_cli_that_is_out_down_or_wrapping_up_says_so() {
    let set = PlanUsageLine::default();
    let mut out = reading("antigravity-cli", "daily", 1.0);
    out.status = PlanUsageStatus::Rejected;
    out.resets_at = Some(datetime!(2026-10-06 15:45 UTC));
    let agy = |reading: PlanUsageReading| cli("agy", "antigravity-cli", vec![reading]);
    let chip = cli_use(&agy(out.clone()), false, &set, None, &clock());
    assert_eq!(
        chip.state,
        CliState::Out {
            until: Some("Tue 3:45pm".into())
        }
    );
    assert_eq!(chip.color, Role::CliAgy);

    let today = PlanUsageReading {
        resets_at: Some(datetime!(2026-09-30 15:45 UTC)),
        ..out.clone()
    };
    let chip = cli_use(&agy(today), false, &set, None, &clock());
    assert_eq!(
        chip.state,
        CliState::Out {
            until: Some("3:45pm".into())
        }
    );
    let no_reset = PlanUsageReading {
        resets_at: None,
        ..out.clone()
    };
    let chip = cli_use(&agy(no_reset), false, &set, None, &clock());
    assert_eq!(chip.state, CliState::Out { until: None });

    // On the wrap-up allowance the agent finishes, so the chip says so rather than out.
    let wrapping_up = PlanUsageReading {
        wrap_up: true,
        ..out
    };
    let chip = cli_use(&agy(wrapping_up), false, &set, None, &clock());
    assert_eq!(chip.state, CliState::WrappingUp);

    let mut codex = cli("Codex", "codex", vec![reading("codex", "five_hour", 0.38)]);
    codex.health = HealthState::Down;
    let chip = cli_use(&codex, false, &set, None, &clock());
    assert_eq!(chip.state, CliState::Down);
    assert!(chip.needs_you());

    let warned = PlanUsageReading {
        status: PlanUsageStatus::Warning,
        used: None,
        ..reading("codex", "five_hour", 0.0)
    };
    let chip = cli_use(
        &cli("Codex", "codex", vec![warned]),
        false,
        &set,
        None,
        &clock(),
    );
    assert_eq!(chip.state, CliState::NearLimit);
}

/// A reading whose window has reset is dropped, and one that couldn't be read
/// again says how old it is.
#[test]
fn a_reading_is_dropped_once_its_window_resets_and_says_its_age_before() {
    let set = PlanUsageLine::default();
    let mut five = reading("antigravity-cli", "five_hour", 0.80);
    five.resets_at = Some(T0 + time::Duration::hours(2));
    let week = reading("antigravity-cli", "seven_day", 0.30);
    let agy = cli("agy", "antigravity-cli", vec![five, week]);

    let fresh = cli_use(&agy, false, &set, None, &clock());
    assert_eq!(shown(&fresh), [("5h".to_owned(), 80)]);
    assert_eq!(fresh.windows[0].age, None);

    // Two days on, the five-hour window has long reset, so only the week's reading is left,
    // and it says how old it is.
    let later = Clock {
        now: T0 + time::Duration::days(2) + time::Duration::hours(1),
        ..clock()
    };
    let stale = cli_use(&agy, false, &set, None, &later);
    assert_eq!(shown(&stale), [("7d".to_owned(), 30)]);
    assert_eq!(stale.windows[0].age.as_deref(), Some("2d"));
    let hours = Clock {
        now: T0 + time::Duration::hours(3),
        ..clock()
    };
    let aged = cli_use(
        &cli(
            "agy",
            "antigravity-cli",
            vec![reading("antigravity-cli", "seven_day", 0.3)],
        ),
        false,
        &set,
        None,
        &hours,
    );
    assert_eq!(aged.windows[0].age.as_deref(), Some("3h"));

    // Out until a reset that has passed is no longer out.
    let mut out = reading("codex", "five_hour", 1.0);
    out.status = PlanUsageStatus::Rejected;
    out.resets_at = Some(T0 + time::Duration::hours(1));
    let codex = cli("Codex", "codex", vec![out]);
    assert!(matches!(
        cli_use(&codex, false, &set, None, &clock()).state,
        CliState::Out { .. }
    ));
    assert_eq!(
        cli_use(&codex, false, &set, None, &later).state,
        CliState::Fine
    );
}

#[test]
fn a_cli_nothing_was_read_for_keeps_its_name_and_one_without_a_plan_its_spend() {
    let set = PlanUsageLine::default();
    let codex = cli_use(
        &cli("Codex", "codex", Vec::new()),
        false,
        &set,
        Some(0.4),
        &clock(),
    );
    assert_eq!((codex.windows.len(), codex.note.as_deref()), (0, None));
    assert_eq!(codex.state, CliState::Fine);

    let unreadable = PlanUsageReading {
        status: PlanUsageStatus::Unknown,
        used: None,
        ..reading("codex", "five_hour", 0.0)
    };
    let codex = cli_use(
        &cli("Codex", "codex", vec![unreadable]),
        false,
        &set,
        None,
        &clock(),
    );
    assert!(codex.windows.is_empty());

    let openrouter = cli("OpenRouter", "openrouter", Vec::new());
    let chip = cli_use(&openrouter, false, &set, Some(0.06), &clock());
    assert_eq!(chip.note.as_deref(), Some("$0.06"));
    assert_eq!(chip.color, Role::CatOther);
    let local = cli_use(
        &cli("Local", "ollama", Vec::new()),
        false,
        &set,
        None,
        &clock(),
    );
    assert_eq!(local.note, None);
}

fn status(main: &str, plan_chips: Vec<PlanUsageChip>) -> HeaderAndBottomLine {
    HeaderAndBottomLine {
        title: String::new(),
        repo: "~/code".into(),
        workflow: "captain".into(),
        ultracode: false,
        ultra: None,
        run: None,
        main_agent: main.parse().unwrap(),
        plan_chips,
        line: Default::default(),
        spent: None,
        limit: None,
        narrator: NarratorMode::Off,
        approval_mode: ApprovalMode::Auto,
        awake: None,
    }
}

fn chip(label: &str, windows: Vec<PlanUsageReading>) -> PlanUsageChip {
    PlanUsageChip {
        label: label.into(),
        reading: binding_reading(&windows).cloned(),
        health: HealthState::Healthy,
        windows,
    }
}

/// Claude, Codex and agy, every one of them on the line.
fn crew_status(main: &str) -> HeaderAndBottomLine {
    let mut status = crew_of_three(main);
    status.line.settings = every_cli();
    status
}

fn every_cli() -> BottomLineSettings {
    BottomLineSettings::from_toml("[plan-usage]\nshow = \"all\"").unwrap()
}

/// Claude, Codex and agy, with the line as it ships.
fn crew_of_three(main: &str) -> HeaderAndBottomLine {
    status(
        main,
        vec![
            chip(
                "Claude Code",
                vec![
                    reading("claude-code", "five_hour", 0.01),
                    reading("claude-code", "seven_day", 0.56),
                ],
            ),
            chip(
                "Codex",
                vec![
                    reading("codex", "five_hour", 0.01),
                    reading("codex", "seven_day", 0.11),
                ],
            ),
            chip("agy", vec![reading("antigravity-cli", "five_hour", 0.02)]),
        ],
    )
}

fn built(state: &AppState) -> BottomLine {
    build(state, &UiSettings::default(), &BTreeMap::new(), clock().now)
}

fn chips(line: &BottomLine) -> Vec<(String, bool, Vec<Shown>)> {
    line.clis()
        .map(|cli| (cli.name.clone(), cli.main, shown(cli)))
        .collect()
}

/// As it ships the line shows the CLI in use, then the one used most over the last 30 days
/// that isn't it, and only the one in use while nothing else was used.
#[test]
fn by_default_the_line_shows_the_cli_in_use_and_the_one_used_most() {
    let names = |main: &str, most_used: &[&str]| -> Vec<(String, bool)> {
        let mut state = AppState::default();
        let mut status = crew_of_three(main);
        status.line.most_used = most_used.iter().map(|cli| (*cli).to_owned()).collect();
        state.status = Some(status);
        chips(&built(&state))
            .into_iter()
            .map(|(name, main, _)| (name, main))
            .collect()
    };
    let chip = |name: &str, main: bool| (name.to_owned(), main);
    assert_eq!(
        names("claude-code", &["claude-code", "codex", "antigravity-cli"]),
        [chip("Claude Code", true), chip("Codex", false)]
    );
    assert_eq!(
        names("claude-code", &["antigravity-cli"]),
        [chip("Claude Code", true), chip("agy", false)]
    );
    assert_eq!(
        names("codex/gpt-6.1-sol", &["codex", "claude-code"]),
        [chip("Codex", true), chip("Claude Code", false)]
    );
    // One used most that the crew has no chip for gives way to the next.
    assert_eq!(
        names("claude-code", &["openrouter", "codex"]),
        [chip("Claude Code", true), chip("Codex", false)]
    );
    assert_eq!(names("claude-code", &[]), [chip("Claude Code", true)]);
}

/// Once you pick the line's CLIs, it shows those in your order after the one in use, each
/// with the windows you gave it, and one the crew leaves out with what is known of it.
#[test]
fn a_saved_list_shows_in_its_order_after_the_cli_in_use() {
    let window = |label: &str, used| (label.to_owned(), used);
    let saved = BottomLineSettings::from_toml(
        "[plan-usage]\nclis = [\n  { cli = \"antigravity-cli\" },\n  { cli = \"codex\", windows = \"both\" },\n  { cli = \"openrouter\" },\n]",
    )
    .unwrap();
    let mut state = AppState::default();
    let mut status = crew_of_three("claude-code");
    status.line.settings = saved.clone();
    status.line.most_used = vec!["codex".into()];
    state.status = Some(status);
    assert_eq!(
        chips(&built(&state)),
        [
            (
                "Claude Code".to_owned(),
                true,
                vec![window("5h", 1), window("7d", 56)]
            ),
            ("agy".to_owned(), false, vec![window("5h", 2)]),
            (
                "Codex".to_owned(),
                false,
                vec![window("5h", 1), window("7d", 11)]
            ),
            ("OpenRouter".to_owned(), false, Vec::new()),
        ]
    );

    // The one in use comes first, wherever the list has it.
    let mut status = crew_of_three("codex");
    status.line.settings = saved;
    state.status = Some(status);
    let names: Vec<String> = chips(&built(&state))
        .into_iter()
        .map(|(name, _, _)| name)
        .collect();
    assert_eq!(names, ["Codex", "agy", "OpenRouter"]);
}

/// The mark and the second window follow the main session from Claude Code to Codex, and the
/// crew keeps its order.
#[test]
fn the_mark_and_the_second_window_follow_the_main_session() {
    let window = |label: &str, used| (label.to_owned(), used);
    let mut state = AppState::default();
    state.status = Some(crew_status("claude-code/claude-opus-5-5"));
    let line = built(&state);
    assert_eq!(line.left, [LinePart::Mode]);
    assert_eq!(
        chips(&line),
        [
            (
                "Claude Code".to_owned(),
                true,
                vec![window("5h", 1), window("7d", 56)]
            ),
            ("Codex".to_owned(), false, vec![window("7d", 11)]),
            ("agy".to_owned(), false, vec![window("5h", 2)]),
        ]
    );

    state.status = Some(crew_status("codex/gpt-6.1-sol"));
    assert_eq!(
        chips(&built(&state)),
        [
            ("Claude Code".to_owned(), false, vec![window("7d", 56)]),
            (
                "Codex".to_owned(),
                true,
                vec![window("5h", 1), window("7d", 11)]
            ),
            ("agy".to_owned(), false, vec![window("5h", 2)]),
        ]
    );
}

/// Gauges are one cell a window, so every CLI shows both of its own, the mark still on one.
#[test]
fn gauges_show_both_windows_of_every_cli() {
    let mut state = AppState::default();
    let mut status = crew_status("claude-code");
    status.line.settings =
        BottomLineSettings::from_toml("form = \"gauges\"\n[plan-usage]\nshow = \"all\"").unwrap();
    state.status = Some(status);
    let windows: Vec<(String, bool, usize)> = built(&state)
        .clis()
        .map(|cli| (cli.name.clone(), cli.main, cli.windows.len()))
        .collect();
    assert_eq!(
        windows,
        [
            ("Claude Code".to_owned(), true, 2),
            ("Codex".to_owned(), false, 2),
            ("agy".to_owned(), false, 1),
        ]
    );
}

#[test]
fn the_main_cli_has_a_chip_even_outside_the_crew_and_newer_readings_win() {
    let mut state = AppState::default();
    let mut status = crew_status("opencode");
    status.plan_chips.remove(0);
    state.status = Some(status);
    // A run's event read Codex's week again, and found a CLI the controller didn't list.
    let mut newer = reading("codex", "seven_day", 0.42);
    newer.observed_at = T0 + time::Duration::seconds(30);
    state.plan_windows.insert("codex".into(), vec![newer]);
    state.plan_windows.insert(
        "claude-code".into(),
        vec![reading("claude-code", "five_hour", 0.2)],
    );
    let names: Vec<_> = chips(&built(&state))
        .into_iter()
        .map(|(name, main, windows)| (name, main, windows.last().map(|(_, used)| *used)))
        .collect();
    assert_eq!(
        names,
        [
            ("OpenCode".to_owned(), true, None),
            ("Codex".to_owned(), false, Some(42)),
            ("agy".to_owned(), false, Some(2)),
            ("Claude Code".to_owned(), false, Some(20)),
        ]
    );
}

#[test]
fn the_settings_pick_the_parts_their_sides_and_who_shows() {
    let mut state = AppState::default();
    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml(
        "form = \"compact\"\nleft = [\"mode\", \"artifact\", \"branch\"]\n\
         right = [\"spend\", \"plan-usage\", \"awake\"]\n\
         [plan-usage]\nshow = \"main\"\nas = \"left\"\n[narrow]\ndrop = [\"branch\"]\n",
    )
    .unwrap();
    status.line.branch = Some("main".into());
    status.line.spend_days = vec![0.0, 0.2, 0.3, 0.05, 0.5, 0.8, 0.01];
    status.line.artifact = Some(PublishedArtifact {
        name: "weekly-usage-chart".into(),
        others: 2,
    });
    state.status = Some(status);
    let mut slots = BTreeMap::new();
    slots.insert(
        "awake".to_owned(),
        PartBody::Words {
            text: "awake 2h".into(),
            tone: Tone::Dim,
        },
    );
    let line = build(&state, &UiSettings::default(), &slots, clock().now);
    assert_eq!(
        line.form,
        argonauts_core::settings::bottom_line::LineForm::Compact
    );
    assert!(line.as_left);
    assert_eq!(line.drop_first, ["branch"]);
    let names = |parts: &[LinePart]| -> Vec<String> {
        parts
            .iter()
            .map(|part| match part {
                LinePart::Mode => "mode".to_owned(),
                LinePart::PlanUsage(clis) => format!("plan-usage:{}", clis.len()),
                LinePart::Added(part) => part.name.clone(),
            })
            .collect()
    };
    assert_eq!(names(&line.left), ["mode", "artifact", "branch"]);
    assert_eq!(names(&line.right), ["spend", "plan-usage:1", "awake"]);
    let LinePart::Added(spend) = &line.right[0] else {
        panic!("the spend part");
    };
    assert_eq!(
        spend.body,
        PartBody::Spark {
            text: "~$1.86".into(),
            values: vec![0.0, 0.2, 0.3, 0.05, 0.5, 0.8, 0.01],
            role: Role::CliClaude,
        }
    );
    let LinePart::Added(artifact) = &line.left[1] else {
        panic!("the artifact part");
    };
    assert_eq!(
        artifact.body,
        PartBody::Artifact {
            name: "weekly-usage-chart".into(),
            others: 2,
        }
    );
}

/// The `run` mod on the line is the workflow under way: its name and how many of its tasks
/// are merged, from the plan's first task until the run ends. A cancelled task doesn't count.
#[test]
fn the_run_mod_draws_the_workflow_under_way() {
    use crate::frame::state::{RunState, TaskRow};
    use argonauts_core::runs::events::run::{RunNamed, RunOutcome};
    use argonauts_core::runs::events::task::TaskState;
    use argonauts_core::settings::workflows::RunStyle;
    use argonauts_core::{RunId, TaskId};

    let run_id = RunId::new("run-20260930-101500-3fa2").unwrap();
    let mut status = crew_status("claude-code");
    status.line.settings =
        BottomLineSettings::from_toml("right = [\"run\", \"plan-usage\"]").unwrap();
    let mut state = AppState::default();
    state.status = Some(status);
    state.attached = Some(run_id.clone());
    let mut run = RunState::new(run_id.clone());
    run.title = "add due dates to every task".into();
    state.runs.insert(run_id.clone(), run);
    let part = |state: &AppState| match built(state).right.first() {
        Some(LinePart::Added(part)) if part.name == RUN => Some(part.body.clone()),
        _ => None,
    };
    assert_eq!(part(&state), None, "no tasks to count yet");

    let states = [
        Some(TaskState::Merged),
        Some(TaskState::Merged),
        Some(TaskState::Merged),
        None,
        None,
        Some(TaskState::Cancelled),
    ];
    let run = state.runs.get_mut(&run_id).unwrap();
    for (n, task_state) in (1..).zip(states) {
        let mut task = TaskRow::new(TaskId::numbered(n), "a task");
        task.state = task_state;
        run.tasks.insert(task.id.clone(), task);
    }
    let meter = |label: &str| PartBody::Meter {
        label: label.into(),
        label_role: Role::BarFill,
        ratio: 0.6,
        value: "3/5".into(),
        fill: Role::BarFill,
    };
    assert_eq!(part(&state), Some(meter("add due dates to every task")));

    let run = state.runs.get_mut(&run_id).unwrap();
    run.named = Some(RunNamed {
        name: "add-due-dates".into(),
        description: "Add due dates to every task".into(),
        style: RunStyle::Workflow,
        based_on: None,
        done_when: Vec::new(),
    });
    assert_eq!(part(&state), Some(meter("add-due-dates")));

    // What other code drew for the name wins, and a finished run leaves the line.
    let mut slots = BTreeMap::new();
    let drawn = PartBody::Drawn(vec![ratatui::text::Span::raw("add-due-dates 3/5")]);
    slots.insert(RUN.to_owned(), drawn.clone());
    let line = build(&state, &UiSettings::default(), &slots, clock().now);
    assert_eq!(
        line.right.first(),
        Some(&LinePart::Added(AddedPart {
            name: RUN.to_owned(),
            body: drawn
        }))
    );
    state.runs.get_mut(&run_id).unwrap().finished = Some(RunOutcome::Merged);
    assert_eq!(part(&state), None);
}

/// The spend part is the last seven days, a run on screen or not. A run with a limit shows
/// its own spend against it, since that is the number that can stop it.
#[test]
fn the_spend_part_is_the_week_unless_the_run_has_a_limit() {
    use crate::frame::state::RunState;
    use argonauts_core::RunId;

    let run_id = RunId::new("run-20260930-101500-3fa2").unwrap();
    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("right = [\"spend\"]").unwrap();
    status.line.spend_days = vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.42, 1.44];
    let mut state = AppState::default();
    state.status = Some(status);
    state.attached = Some(run_id.clone());
    let mut run = RunState::new(run_id.clone());
    run.spent.cost_usd = 0.21;
    state.runs.insert(run_id, run);
    let spend = |state: &AppState| match built(state).right.first() {
        Some(LinePart::Added(part)) => part.body.clone(),
        other => panic!("the spend part, not {other:?}"),
    };
    assert_eq!(
        spend(&state),
        PartBody::Spark {
            text: "~$1.86".into(),
            values: vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.42, 1.44],
            role: Role::CliClaude,
        }
    );

    state.status.as_mut().unwrap().limit = Some(LimitAmount {
        unit: LimitUnit::Usd,
        amount: 5.0,
    });
    assert_eq!(
        spend(&state),
        PartBody::Words {
            text: "$0.21/$5.00".into(),
            tone: Tone::Dim
        }
    );

    // With no day on record yet, the run's own spend stands in.
    let status = state.status.as_mut().unwrap();
    status.limit = None;
    status.line.spend_days.clear();
    assert_eq!(
        spend(&state),
        PartBody::Words {
            text: "$0.21".into(),
            tone: Tone::Dim
        }
    );
}

/// The artifact part is on the line as it ships, and draws only once a session published one.
#[test]
fn parts_with_nothing_to_show_draw_nothing() {
    let mut state = AppState::default();
    state.status = Some(crew_status("claude-code"));
    let line = built(&state);
    assert_eq!(line.left, [LinePart::Mode]);
    assert_eq!(line.right.len(), 1, "plan usage alone");
    let unset = built(&AppState::default());
    assert_eq!(
        (unset.left, unset.right),
        (vec![LinePart::Mode], Vec::new())
    );
}

/// A turn's own reading shows before the controller's first status names the crew.
#[test]
fn a_conversation_turns_reading_shows_before_any_status() {
    let mut state = AppState::default();
    state.plan_windows.insert(
        "claude-code".into(),
        vec![reading("claude-code", "five_hour", 0.71)],
    );
    assert_eq!(
        chips(&built(&state)),
        [("Claude Code".to_owned(), false, vec![("5h".to_owned(), 71)])]
    );
}

/// Before the first status, the line shows only the CLI your settings start the main session
/// on, as the welcome names it, rather than every CLI a turn read and then fewer.
#[test]
fn before_any_status_the_line_shows_the_cli_the_settings_name() {
    let mut state = AppState::default();
    state.plan_windows.insert(
        "claude-code".into(),
        vec![reading("claude-code", "five_hour", 0.71)],
    );
    state
        .plan_windows
        .insert("codex".into(), vec![reading("codex", "seven_day", 0.84)]);
    state.welcome = Some(crate::frame::state::Welcome {
        version: "0.2.0".to_owned(),
        repo: "/Users/you/code/tasks-cli".to_owned(),
        workflow: "captain".to_owned(),
        main_agent: Some(argonauts_core::base::names::ModelRef::new(
            "claude-code",
            None,
        )),
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
    });
    let shown: Vec<String> = chips(&built(&state))
        .into_iter()
        .map(|(name, _, _)| name)
        .collect();
    assert_eq!(shown, ["Claude Code"]);
}

/// A Claude Code mod's status text nobody placed still shows, last and first to go, unless
/// the settings hide mods' output.
#[test]
fn a_mods_status_text_sits_last_unless_you_place_it() {
    let mut state = AppState::default();
    state.status = Some(crew_status("claude-code"));
    state.conversation.plugin_status = vec![("weather".to_owned(), "sunny".to_owned())];
    let line = built(&state);
    let Some(LinePart::Added(last)) = line.right.last() else {
        panic!("the mod's text");
    };
    assert_eq!(*last, AddedPart::words("weather", "sunny", Tone::Dim));
    assert_eq!(line.drop_first.first().map(String::as_str), Some("weather"));

    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("left = [\"mode\", \"weather\"]").unwrap();
    state.status = Some(status);
    let line = built(&state);
    assert_eq!(line.left.len(), 2);
    assert_eq!(line.right.len(), 1, "placed once, on the left");

    let hidden = UiSettings {
        plugin_output: PluginOutput::Off,
        ..UiSettings::default()
    };
    let line = build(&state, &hidden, &BTreeMap::new(), clock().now);
    assert_eq!(line.left, [LinePart::Mode]);
}

/// A hold on the computer's sleep that you asked for shows while it lasts, before the chips,
/// even where the settings name no `awake` part. Named there, it sits where they put it.
#[test]
fn a_hold_on_sleep_shows_wherever_the_settings_leave_it_out() {
    let mut state = AppState::default();
    state.status = Some(crew_status("claude-code"));
    let held = PartBody::Words {
        text: "☕".to_owned(),
        tone: Tone::Code,
    };
    let slots = BTreeMap::from([(AWAKE.to_owned(), held.clone())]);
    let with_hold = |state: &AppState| build(state, &UiSettings::default(), &slots, clock().now);
    let awake = LinePart::Added(AddedPart {
        name: AWAKE.to_owned(),
        body: held,
    });

    let line = with_hold(&state);
    assert_eq!(line.right.first(), Some(&awake));
    assert!(matches!(line.right.get(1), Some(LinePart::PlanUsage(_))));
    assert!(!line.left.contains(&awake));
    assert!(!built(&state).right.contains(&awake), "nothing holds it");

    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("left = [\"mode\", \"awake\"]").unwrap();
    state.status = Some(status);
    let line = with_hold(&state);
    assert_eq!(line.left, [LinePart::Mode, awake.clone()]);
    assert!(!line.right.contains(&awake), "placed once, on the left");
}

#[test]
fn reset_times_read_as_claude_code_writes_them() {
    let at = |time: OffsetDateTime| clock_time(time, &clock());
    assert_eq!(at(datetime!(2026-09-30 16:40 UTC)), "4:40pm");
    assert_eq!(at(datetime!(2026-09-30 00:05 UTC)), "12:05am");
    assert_eq!(at(datetime!(2026-09-30 12:00 UTC)), "12:00pm");
    assert_eq!(at(datetime!(2026-10-01 09:00 UTC)), "Thu 9:00am");
    let tokyo = Clock {
        utc_offset: UtcOffset::from_hms(9, 0, 0).unwrap(),
        ..clock()
    };
    assert_eq!(
        clock_time(datetime!(2026-09-30 16:40 UTC), &tokyo),
        "Thu 1:40am"
    );
}

/// A page the main agent publishes with Claude Code's Artifact tool shows on the line beside
/// the mode, named by the newest one with how many came before it.
#[test]
fn a_page_the_conversation_published_names_the_artifact_part() {
    use argonauts_core::AgentEvent;
    use argonauts_core::app::{AppEvent, ConversationEvent};
    use argonauts_core::base::names::TurnId;

    let mut state = AppState::default();
    state.status = Some(crew_status("claude-code"));
    let said = |event| AppEvent::Conversation {
        event: ConversationEvent::Agent {
            turn: TurnId::numbered(1),
            event,
        },
    };
    state.apply(&AppEvent::Conversation {
        event: ConversationEvent::TurnStarted {
            turn: TurnId::numbered(1),
            text: "write it up".into(),
            worker: "claude-code/opus".parse().unwrap(),
            read_only: false,
            commit: "abc".into(),
            uncommitted: Vec::new(),
            settings_sha256: None,
            continues: false,
            opens_a_talk: false,
        },
    });
    for (id, file) in [("t1", "test-report"), ("t2", "weekly-usage-chart")] {
        state.apply(&said(AgentEvent::ToolStarted {
            id: id.into(),
            name: "Artifact".into(),
            input: serde_json::json!({"file_path": format!("out/{file}.html")}),
            parent: None,
        }));
        state.apply(&said(AgentEvent::ToolFinished {
            id: id.into(),
            ok: true,
            output: Some(format!(
                "Published out/{file}.html at https://claude.ai/code/artifact/{id} (Version 1)"
            )),
            parent: None,
        }));
    }
    let line = built(&state);
    let artifact = line.left.iter().find_map(|part| match part {
        LinePart::Added(added) if added.name == "artifact" => Some(added.body.clone()),
        _ => None,
    });
    assert_eq!(
        artifact,
        Some(PartBody::Artifact {
            name: "weekly-usage-chart".into(),
            others: 1,
        }),
        "{line:?}"
    );
}

/// A mod of yours from `mods/` that the line lists draws its chip there, with the theme.
#[test]
fn a_mod_of_yours_the_line_lists_is_its_chip() {
    use argonauts_core::settings::mods::view::{ModRow, ModRowKind, ModsView};
    use argonauts_core::settings::mods::{
        ModBody, ModOrigin, ModPlace, ModReading, builtin, check,
    };

    let (name, text) = builtin::STARTERS
        .iter()
        .find(|(starter, _)| *starter == "checks")
        .unwrap();
    let mut file = check::read(name, text).unwrap();
    file.places = vec![ModPlace::BottomLine];
    let reading = ModReading::sample(ModBody::Stack);
    let mut state = AppState::default();
    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("left = [\"mode\", \"checks\"]").unwrap();
    state.status = Some(status);
    state.mods = Some(ModsView {
        rows: vec![ModRow {
            name: (*name).to_owned(),
            origin: ModOrigin::Yours,
            path: format!("~/.argonauts/mods/{name}.toml"),
            kind: ModRowKind::Card {
                file: Box::new(file.clone()),
                built_in: None,
                reading: Some(reading.clone()),
                note: None,
                needs_trust: false,
                asks_for_line: false,
                hidden_by_line: false,
            },
        }],
        ..ModsView::default()
    });
    let line = built(&state);
    assert_eq!(
        line.left[1],
        LinePart::Added(AddedPart {
            name: "checks".into(),
            body: PartBody::Mod {
                file: Box::new(file),
                reading: Box::new(reading),
            },
        })
    );
}

/// The `stats` part says what the stats mod says from a hidden band, once the numbers carry
/// the crew's stats, and nothing before.
#[test]
fn the_stats_part_says_the_periods_tokens() {
    use crate::testing::sample;
    use std::sync::Arc;

    let mut status = crew_status("claude-code");
    status.line.settings = BottomLineSettings::from_toml("right = [\"stats\"]").unwrap();
    let mut state = AppState::default();
    state.status = Some(status);
    assert_eq!(built(&state).right, [], "no numbers yet");

    let mut usage = sample::usage();
    usage.stats = Some(sample::crew_stats());
    state.usage = Some(Arc::new(usage));
    state.names = sample::names();
    let Some(LinePart::Added(part)) = built(&state).right.first().cloned() else {
        panic!("the stats part");
    };
    assert_eq!(part.name, "stats");
    let PartBody::Words { text, tone } = part.body else {
        panic!("words, not {:?}", part.body);
    };
    assert!(
        text.starts_with("stats ") && text.ends_with(" tok"),
        "{text}"
    );
    assert_eq!(tone, Tone::Dim);
}

/// The `context` part of a line built from `state`.
fn context_part(state: &AppState) -> Option<PartBody> {
    built(state).left.iter().find_map(|part| match part {
        LinePart::Added(part) if part.name == "context" => Some(part.body.clone()),
        _ => None,
    })
}

fn fill(who: &str, percent: u32, level: crate::base::kit::Level) -> Option<PartBody> {
    Some(PartBody::Fill {
        label: "context".to_owned(),
        who: who.to_owned(),
        who_role: Role::Label,
        percent,
        level,
    })
}

/// A run on screen with a working session for each `(label, used, cap)`.
fn with_sessions(state: &mut AppState, sessions: &[(&str, u64, u64)]) -> argonauts_core::RunId {
    use crate::frame::state::{RunState, SessionState, SessionStatus};
    use argonauts_core::app::views::ContextUse;
    use argonauts_core::base::names::{RoleName, SessionKey};

    let run_id = argonauts_core::RunId::new("run-20260930-101500-3fa2").unwrap();
    let mut run = RunState::new(run_id.clone());
    for (n, (label, used, cap)) in (1..).zip(sessions) {
        let key = SessionKey::numbered(n);
        let worker = "claude-code/claude-sonnet-5-5".parse().unwrap();
        let started = time::OffsetDateTime::UNIX_EPOCH;
        let mut session = SessionState::new(
            key.clone(),
            worker,
            "claude-code",
            RoleName::Worker,
            started,
        );
        session.label = Some((*label).to_owned());
        session.status = SessionStatus::Working;
        session.context = Some(ContextUse {
            used: *used,
            window: Some(*cap),
            cap: Some(*cap),
        });
        run.session_order.push(key.clone());
        run.sessions.insert(key, session);
    }
    state.attached = Some(run_id.clone());
    state.runs.insert(run_id.clone(), run);
    run_id
}

fn set_used(state: &mut AppState, run: &argonauts_core::RunId, label: &str, used: u64) {
    let sessions = &mut state.runs.get_mut(run).unwrap().sessions;
    let session = sessions
        .values_mut()
        .find(|session| session.label.as_deref() == Some(label))
        .unwrap();
    session.context.as_mut().unwrap().used = used;
}

/// The conversation's count: `total` tokens in a 1M window that compacts at 967k.
fn counted(total: u64) -> crate::frame::state::ContextCount {
    use argonauts_core::runs::events::context::ContextUsage;
    crate::frame::state::ContextCount {
        usage: ContextUsage {
            total_tokens: total,
            window: 1_000_000,
            raw_window: 1_000_000,
            auto_compact: true,
            auto_compact_threshold: Some(967_000),
            ..ContextUsage::default()
        },
        counted: time::OffsetDateTime::UNIX_EPOCH,
    }
}

/// With no run on screen, the `context` part is the main session's, against where its CLI
/// compacts.
#[test]
fn the_context_part_names_the_main_session_without_a_run() {
    use crate::base::kit::Level;
    let mut state = AppState::default();
    let mut status = crew_status("claude-code");
    status.line.settings =
        BottomLineSettings::from_toml("left = [\"mode\", \"context\"]\n").unwrap();
    state.status = Some(status);
    assert_eq!(context_part(&state), None, "nothing counted yet");
    state.conversation.context = Some(counted(609_210));
    assert_eq!(context_part(&state), fill("main", 63, Level::Ok));
    state.conversation.context = Some(counted(944_619));
    assert_eq!(context_part(&state), fill("main", 98, Level::Danger));
}

/// With a run on screen the part names its busiest live session, against where that starts
/// afresh. The main session's shows only while it alone is close to compacting.
#[test]
fn the_context_part_names_the_busiest_agent_unless_only_the_main_session_is_close() {
    use crate::base::kit::Level;
    let mut state = AppState::default();
    let mut status = crew_status("claude-code");
    status.line.settings =
        BottomLineSettings::from_toml("left = [\"mode\", \"context\"]\n").unwrap();
    state.status = Some(status);
    let run_id = with_sessions(
        &mut state,
        &[
            ("build:due-flag", 36_200, 180_000),
            ("build:due-tests", 13_400, 244_800),
        ],
    );
    state.conversation.context = Some(counted(750_000));
    assert_eq!(context_part(&state), fill("build:due-flag", 20, Level::Ok));

    // Both are close: the agent's stays.
    set_used(&mut state, &run_id, "build:due-flag", 171_400);
    state.conversation.context = Some(counted(944_619));
    assert_eq!(
        context_part(&state),
        fill("build:due-flag", 95, Level::Danger)
    );

    // Only the main session is close.
    set_used(&mut state, &run_id, "build:due-flag", 36_200);
    assert_eq!(context_part(&state), fill("main", 98, Level::Danger));
    state.conversation.context = Some(counted(800_000));
    assert_eq!(context_part(&state), fill("main", 83, Level::Warn));
}

/// A run under way with one task on `worker`, in `state`, and the task's own row to change.
fn task_on<'a>(state: &'a mut AppState, worker: &str) -> &'a mut crate::frame::state::TaskRow {
    use crate::frame::state::{RunState, TaskRow};

    let run_id = argonauts_core::RunId::new("run-20260930-101500-7c1e").unwrap();
    let task = argonauts_core::TaskId::new("due-flag").unwrap();
    let mut row = TaskRow::new(task.clone(), "Add a due flag");
    row.worker = Some(worker.parse().unwrap());
    row.state = Some(TaskState::Running);
    let mut run = RunState::new(run_id.clone());
    run.tasks.insert(task.clone(), row);
    state.runs.insert(run_id.clone(), run);
    state
        .runs
        .get_mut(&run_id)
        .and_then(|run| run.tasks.get_mut(&task))
        .unwrap()
}

/// Claude in use and Codex used most, with agy's reading `agy`.
fn with_agy(agy: PlanUsageReading) -> AppState {
    let mut state = AppState::default();
    let mut status = crew_of_three("claude-code");
    status.plan_chips[2] = chip("agy", vec![agy]);
    status.line.most_used = vec!["codex".into()];
    state.status = Some(status);
    state
}

fn names(state: &AppState) -> Vec<String> {
    chips(&built(state))
        .into_iter()
        .map(|(name, _, _)| name)
        .collect()
}

/// A CLI the line leaves out joins it only while a running task waits on it and it's out,
/// down or at 90% of a window, and then with its reset time, whatever `resets` says.
#[test]
fn a_cli_joins_while_a_running_task_waits_on_it_and_it_is_held_back() {
    let mut out = reading("antigravity-cli", "five_hour", 1.0);
    out.status = PlanUsageStatus::Rejected;
    out.resets_at = Some(datetime!(2026-10-06 15:45 UTC));

    // Out, and nothing waits on it.
    let mut state = with_agy(out.clone());
    assert_eq!(names(&state), ["Claude Code", "Codex"]);

    // A task waits for it to come back.
    task_on(&mut state, "codex/gpt-5.6-luna").waiting_on_agent =
        Some(crate::frame::state::AgentWait {
            agent_id: "antigravity-cli".into(),
            until: out.resets_at,
        });
    let line = built(&state);
    assert_eq!(names(&state), ["Claude Code", "Codex", "agy"]);
    let agy = line.clis().last().unwrap();
    assert_eq!(
        agy.state,
        CliState::Out {
            until: Some("Tue 3:45pm".into())
        }
    );

    // Fine, while a task works on it: it stays off.
    let mut state = with_agy(reading("antigravity-cli", "five_hour", 0.4));
    task_on(&mut state, "antigravity-cli/gemini-3.8-flash");
    assert_eq!(names(&state), ["Claude Code", "Codex"]);

    // At 93% while a task works on it, it joins with when the window resets.
    let mut near = reading("antigravity-cli", "five_hour", 0.93);
    near.resets_at = Some(datetime!(2026-09-30 16:40 UTC));
    let mut state = with_agy(near.clone());
    if let Some(status) = state.status.as_mut() {
        status.line.settings =
            BottomLineSettings::from_toml("[plan-usage]\nresets = \"never\"").unwrap();
    }
    task_on(&mut state, "antigravity-cli/gemini-3.8-flash");
    let line = built(&state);
    let agy = line.clis().last().unwrap();
    assert_eq!(agy.name, "agy");
    assert_eq!(agy.windows[0].resets.as_deref(), Some("4:40pm"));

    // Once the run is over, or with the join turned off, it goes.
    let mut state = with_agy(near.clone());
    task_on(&mut state, "antigravity-cli/gemini-3.8-flash");
    for run in state.runs.values_mut() {
        run.finished = Some(argonauts_core::runs::events::run::RunOutcome::Merged);
    }
    assert_eq!(names(&state), ["Claude Code", "Codex"]);
    let mut state = with_agy(near);
    task_on(&mut state, "antigravity-cli/gemini-3.8-flash");
    if let Some(status) = state.status.as_mut() {
        status.line.settings =
            BottomLineSettings::from_toml("[plan-usage]\nholding-up = false").unwrap();
    }
    assert_eq!(names(&state), ["Claude Code", "Codex"]);
}

/// A CLI that is down joins while an agent of the run is still on it.
#[test]
fn a_cli_that_is_down_joins_while_an_agent_works_on_it() {
    let mut state = AppState::default();
    let mut status = crew_of_three("claude-code");
    status.plan_chips[2].health = HealthState::Down;
    state.status = Some(status);
    assert_eq!(names(&state), ["Claude Code"]);
    let run = with_sessions(&mut state, &[("build:due-flag", 1, 10)]);
    if let Some(session) = state
        .runs
        .get_mut(&run)
        .and_then(|run| run.sessions.values_mut().next())
    {
        session.agent_id = "antigravity-cli".into();
    }
    let line = built(&state);
    assert_eq!(names(&state), ["Claude Code", "agy"]);
    assert_eq!(line.clis().last().unwrap().state, CliState::Down);
}
