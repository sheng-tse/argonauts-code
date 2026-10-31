//! `argonauts telemetry show`: the next report as it will be sent, one row a part, with when it
//! goes and what is never in it.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::base::text::shown_name;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::models::model_catalog::display_name_from_id;
use argonauts_tui::base::theme::{Role, Theme};
use ratatui::text::{Line, Span};

use super::event::{DailyUsage, Fallbacks, ModelUse, RunEnds};
use super::notice::STOP_COMMAND;

/// Where each row's note starts.
const NOTE_COL: usize = 58;
/// The width of the key column, after its indent.
const KEY_WIDTH: usize = 11;

/// The commands named in the features row in this order, then any other.
const COMMANDS: &[&str] = &[
    "/resume",
    "/model",
    "/effort",
    "/workflow",
    "/context",
    "/config",
    "/update",
    "/issue",
];

/// When the next report goes, as the first line says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Due {
    /// At the next start.
    NextStart,
    /// At the first start after this local time, today or tomorrow.
    After { today: bool, clock: String },
    /// The count began today, so the first report goes at a start tomorrow.
    Tomorrow,
}

/// Why nothing goes, when nothing does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Held {
    Off,
    DoNotTrack,
    Ci,
    /// A debug or test build, which never sends.
    DebugBuild,
    /// An optimized build made with `ARGONAUTS_POSTHOG_KEY` set empty, or with a host of its own
    /// and no key.
    NoKey,
}

/// The whole of what `show` prints.
pub(crate) fn lines(
    usage: &DailyUsage,
    due: &Due,
    held: &[Held],
    theme: &Theme,
) -> Vec<Line<'static>> {
    let text = theme.style(Role::Text);
    let dim = theme.style(Role::Dim);
    let mut lines = vec![Line::from(Span::styled(first_line(due, held), text))];
    for why in held.iter().filter(|why| **why != Held::Off) {
        lines.push(Line::from(Span::styled(held_line(*why), dim)));
    }
    lines.push(Line::default());
    for (key, value, note) in rows(usage) {
        let mut spans = vec![
            Span::styled(format!("  {key:<KEY_WIDTH$}"), dim),
            Span::styled(value.clone(), text),
        ];
        if let Some(note) = note {
            let used = 2 + KEY_WIDTH + argonauts_tui::base::text::str_width(&value);
            let gap = NOTE_COL.saturating_sub(used).max(2);
            spans.push(Span::styled(format!("{}{note}", " ".repeat(gap)), dim));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(
            "Never sent: prompts, code, diffs, file or repo names, accounts, keys, IP addresses. ",
            dim,
        ),
        Span::styled(STOP_COMMAND, theme.style(Role::Question)),
        Span::styled(" stops it.", dim),
    ]));
    lines
}

fn first_line(due: &Due, held: &[Held]) -> String {
    if held.contains(&Held::Off) {
        return "Usage reports are off, so nothing is sent; argonauts telemetry on turns them back \
                on. A report would hold exactly this:"
            .to_owned();
    }
    let when = match due {
        Due::NextStart => "Today's report goes out at the next start".to_owned(),
        Due::After { today: true, clock } => {
            format!("Today's report goes out at the first start after {clock}")
        }
        Due::After {
            today: false,
            clock,
        } => {
            format!("Tomorrow's report goes out at the first start after {clock}")
        }
        Due::Tomorrow => "The first report goes out at a start tomorrow".to_owned(),
    };
    format!("Usage reports are on. {when} and holds exactly this:")
}

pub(super) fn held_line(why: Held) -> &'static str {
    match why {
        Held::Off => "",
        Held::DoNotTrack => "DO_NOT_TRACK is set, so nothing is sent while it is.",
        Held::Ci => "CI is set, so nothing is sent while it is.",
        Held::DebugBuild => "This is a debug build, so it sends nothing.",
        Held::NoKey => "This build has no PostHog key, so it sends nothing.",
    }
}

/// Each row: its key, what it holds, and the rest of it set at the note column.
fn rows(usage: &DailyUsage) -> Vec<(&'static str, String, Option<String>)> {
    let mut rows = vec![install_row(usage), setup_row(usage)];
    rows.extend(clis_row(usage));
    let mut models = usage.models.iter();
    match models.next() {
        Some(first) => {
            let (value, note) = model_row(first);
            rows.push(("models", value, Some(note)));
            for model in models {
                let (value, note) = model_row(model);
                rows.push(("", value, Some(note)));
            }
        }
        None => rows.push(("models", "none".to_owned(), None)),
    }
    rows.push(runs_row(usage));
    let tasks = &usage.tasks;
    rows.push((
        "tasks",
        format!("{} approved at the first review", tasks.first_pass),
        Some(format!(
            "{} after one more round · {} after two or more",
            tasks.second_pass, tasks.more_rounds
        )),
    ));
    let findings = &usage.findings;
    let notes = findings.given.saturating_sub(findings.blocking);
    let by_reviewer = counted_names(&findings.by_reviewer);
    rows.push((
        "findings",
        format!(
            "{} ({} blocking, {notes} notes) · {} fixed",
            findings.given, findings.blocking, findings.fixed
        ),
        (!by_reviewer.is_empty()).then(|| format!("by reviewer: {by_reviewer}")),
    ));
    let checks = &usage.checks;
    rows.push((
        "checks",
        format!(
            "{} of {} passed the first try",
            checks.first_try, checks.tasks_checked
        ),
        Some(format!(
            "{} of {} after review",
            checks.after_review, checks.tasks_checked
        )),
    ));
    let merges = &usage.merges;
    let mut merged = vec![
        format!("you {}", merges.you),
        format!("autopilot {}", merges.autopilot),
    ];
    if merges.judge > 0 {
        merged.push(format!("the judge {}", merges.judge));
    }
    if merges.rules > 0 {
        merged.push(format!("your rules {}", merges.rules));
    }
    merged.push(format!("sent back {}", merges.sent_back));
    rows.push(("merges", merged.join(" · "), None));
    rows.push(("fallbacks", fallbacks_value(&usage.fallbacks), None));
    rows.push(features_row(usage));
    rows.push(health_row(usage));
    rows.extend(behaviors_row(usage));
    rows
}

fn install_row(usage: &DailyUsage) -> (&'static str, String, Option<String>) {
    let id: String = usage.install.chars().take(8).collect();
    let machine = &usage.machine;
    let mut about = vec![usage.version.clone(), os_name(&usage.os)];
    about.push(chip_name(&machine.chip));
    if let Some(gb) = machine.ram_gb {
        about.push(format!("{gb} GB"));
    }
    if let Some(cores) = machine.cores {
        about.push(format!("{cores} cores"));
    }
    (
        "install",
        format!("{id}… (random, made on this machine)"),
        Some(about.join(" · ")),
    )
}

fn setup_row(usage: &DailyUsage) -> (&'static str, String, Option<String>) {
    let setup = &usage.setup;
    let value = [
        terminal_name(&usage.machine.terminal).to_owned(),
        setup.screen.clone(),
        format!("sandbox {}", setup.sandbox),
    ];
    let autopilot = if setup.autopilot { "on" } else { "off" };
    let git = if setup.git_repo {
        "git repo"
    } else {
        "no git repo"
    };
    (
        "setup",
        value.join(" · "),
        Some(format!(
            "approvals {} · autopilot {autopilot} · {git}",
            setup.approvals.replace('_', " ")
        )),
    )
}

/// The installed CLIs and their versions, once the report holds any.
fn clis_row(usage: &DailyUsage) -> Option<(&'static str, String, Option<String>)> {
    if usage.clis.is_empty() {
        return None;
    }
    let clis: Vec<String> = usage
        .clis
        .iter()
        .map(|(cli, version)| format!("{} {version}", cli_name(cli)))
        .collect();
    Some(("clis", clis.join(" · "), None))
}

fn model_row(model: &ModelUse) -> (String, String) {
    let billing = match model.billing {
        "api_key" => "API key",
        billing => billing,
    };
    let value = format!(
        "{} · {} · {billing}",
        model_name(&model.model, model.billing),
        cli_name(&model.cli)
    );
    let mut note: Vec<String> = model.roles.iter().cloned().collect();
    note.extend(model.efforts.iter().cloned());
    note.push(format!("{} in", tokens(model.input)));
    note.push(format!("{} out", tokens(model.output)));
    if model.cached > 0 {
        note.push(format!("{} cached", tokens(model.cached)));
    }
    note.push(dollars(model.usd));
    (value, note.join(" · "))
}

fn runs_row(usage: &DailyUsage) -> (&'static str, String, Option<String>) {
    // The workflow that ran most first.
    let mut runs: Vec<(&String, &RunEnds)> = usage
        .runs
        .iter()
        .filter(|(workflow, _)| *workflow != "captain")
        .collect();
    runs.sort_by_key(|(_, ends)| {
        let total: u32 = end_words(ends).iter().map(|(count, _)| count).sum();
        std::cmp::Reverse(total)
    });
    let mut crews = Vec::new();
    for (workflow, ends) in runs {
        for (count, word) in end_words(ends) {
            crews.push(format!("{} {count} {word}", workflow_name(workflow)));
        }
    }
    let captain_runs = usage.runs.get("captain");
    let merged = captain_runs.map_or(0, |ends| ends.merged);
    let mut captain = format!("Captain {} turns", usage.turns);
    if captain_runs.is_some() {
        let workflows = if merged == 1 { "workflow" } else { "workflows" };
        captain.push_str(&format!(", {merged} {workflows} merged"));
    }
    if crews.is_empty() {
        return ("runs", captain, None);
    }
    ("runs", crews.join(" · "), Some(captain))
}

fn end_words(ends: &RunEnds) -> Vec<(u32, &'static str)> {
    [
        (ends.merged, "merged"),
        (ends.pull_request, "to a pull request"),
        (ends.branch_left, "kept on a branch"),
        (ends.abandoned, "dropped"),
        (ends.stopped, "stopped"),
        (ends.failed, "failed"),
        (ends.unknown, "ended"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .collect()
}

fn fallbacks_value(fallbacks: &Fallbacks) -> String {
    let total = fallbacks.total();
    if total == 0 {
        return "none".to_owned();
    }
    let causes: Vec<(u32, &str)> = [
        (fallbacks.signed_out, "signed out"),
        (fallbacks.usage_limit, "usage limit"),
        (fallbacks.down, "down"),
        (fallbacks.model_unavailable, "model unavailable"),
        (fallbacks.stuck, "stuck"),
        (fallbacks.other, "another cause"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .collect();
    let causes = if let [(_, word)] = causes[..] {
        word.to_owned()
    } else {
        causes
            .iter()
            .map(|(count, word)| format!("{word} {count}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let to: BTreeMap<String, u32> = fallbacks
        .to
        .iter()
        .map(|(model, count)| (model_name(model, ""), *count))
        .collect();
    let to = if to.len() == 1 {
        to.into_keys().collect::<String>()
    } else {
        counted_names(&to)
    };
    // The arrow is two cells wide in some fonts, so two spaces keep it off the name.
    format!("{total} · {causes} →  {to}")
}

fn features_row(usage: &DailyUsage) -> (&'static str, String, Option<String>) {
    let features = &usage.features;
    let mut used = vec![
        format!("ultracode {}", features.ultracode),
        format!("mods {}", features.mods),
    ];
    let mut commands: Vec<(&String, &u32)> = features.commands.iter().collect();
    commands.sort_by_key(|(name, _)| {
        let known = COMMANDS.iter().position(|known| known == name);
        (known.unwrap_or(COMMANDS.len()), (*name).clone())
    });
    used.extend(
        commands
            .iter()
            .map(|(name, count)| format!("{name} {count}")),
    );
    (
        "features",
        used.join(" · "),
        Some(format!(
            "workflow view {} · goals {}",
            features.workflow_view, features.goals
        )),
    )
}

fn health_row(usage: &DailyUsage) -> (&'static str, String, Option<String>) {
    let health = &usage.health;
    let crashes: u32 = health.crashes.values().sum();
    let startup = health
        .startup
        .iter()
        .max_by_key(|(_, count)| **count)
        .map(|(bracket, _)| match bracket.as_str() {
            "under_1s" => "start-up under 1 s",
            "1_to_3s" => "start-up 1 to 3 s",
            _ => "start-up over 3 s",
        });
    (
        "health",
        format!(
            "{crashes} crashes · {} outputs not understood",
            health.not_understood
        ),
        startup.map(str::to_owned),
    )
}

/// The behaviors the report counts, once it counts any: how many actions came under how many
/// behaviors, how many no label fits, and how many each model took.
fn behaviors_row(usage: &DailyUsage) -> Option<(&'static str, String, Option<String>)> {
    let counts = &usage.behaviors;
    let mut by_model = counts.behaviors_unlabelled.clone();
    let mut behaviors = BTreeSet::new();
    let mut labelled = 0;
    for (model, roles) in &counts.behaviors {
        for (behavior, count) in roles.values().flatten() {
            behaviors.insert(behavior.as_str());
            labelled += count;
            *by_model.entry(model.clone()).or_default() += count;
        }
    }
    if by_model.is_empty() {
        return None;
    }
    let unlabelled: u32 = counts.behaviors_unlabelled.values().sum();
    Some((
        "behaviors",
        format!(
            "{} in {} · {unlabelled} unlabelled",
            counted(labelled, "action", "actions"),
            counted(behaviors.len() as u32, "behavior", "behaviors")
        ),
        Some(format!("by model: {}", counted_names(&by_model))),
    ))
}

fn counted(count: u32, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// `GPT 6 Luna 6, Gemma series 3`, the most first.
fn counted_names(counts: &BTreeMap<String, u32>) -> String {
    let mut counts: Vec<(&String, &u32)> = counts.iter().collect();
    counts.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    counts
        .iter()
        .map(|(name, count)| format!("{} {count}", model_name(name, "")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A model as the report names it: by its name, a local one by its series.
fn model_name(model: &str, billing: &str) -> String {
    let local = billing == "local" || super::event::is_series(model);
    match model {
        "default" => "its default model".to_owned(),
        "other" => "another model".to_owned(),
        series if local => format!("{} series", shown_name(series)),
        model => display_name_from_id(model),
    }
}

fn cli_name(cli: &str) -> String {
    match cli {
        "endpoint" => "an endpoint".to_owned(),
        cli => DisplayNames::default().cli(cli),
    }
}

fn workflow_name(workflow: &str) -> String {
    match workflow {
        "custom" => "Yours".to_owned(),
        "unknown" => "Unnamed".to_owned(),
        name => shown_name(name),
    }
}

fn os_name(os: &str) -> String {
    match os {
        "macos" => "macOS".to_owned(),
        "linux" => "Linux".to_owned(),
        os => os.to_owned(),
    }
}

fn chip_name(chip: &str) -> String {
    match chip {
        "apple_silicon" => "Apple silicon".to_owned(),
        "intel" => "Intel".to_owned(),
        chip => chip.to_owned(),
    }
}

fn terminal_name(kind: &str) -> &str {
    match kind {
        "ghostty" => "Ghostty",
        "iterm2" => "iTerm2",
        "apple_terminal" => "Terminal",
        "wezterm" => "WezTerm",
        "vscode" => "VS Code",
        "warp" => "Warp",
        "tmux" => "tmux",
        "kitty" => "kitty",
        "alacritty" => "Alacritty",
        "windows_terminal" => "Windows Terminal",
        _ => "another terminal",
    }
}

/// `412.3k`, `2.9m`, or the count itself under a thousand.
fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}m", n as f64 / 1_000_000.0),
    }
}

fn dollars(usd: f64) -> String {
    if usd == 0.0 {
        "$0".to_owned()
    } else {
        format!("${usd:.2}")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::super::event::{
        Checks, Features, Findings, Health, Machine, Merges, SetupFacts, Tasks,
    };
    use super::*;
    use crate::behaviors::report::BehaviorCounts;
    use crate::testing::golden::{assert_looks, drawn_rows, picture_rows, theme};

    const PICTURE: &str = include_str!("../../tests/golden/telemetry_show.txt");
    const COLOURS: &str = include_str!("../../tests/golden/telemetry_show.html");

    fn model(
        cli: &str,
        model: &str,
        billing: &'static str,
        role: &str,
        effort: Option<&str>,
        (input, output, cached): (u64, u64, u64),
        usd: f64,
    ) -> ModelUse {
        ModelUse {
            cli: cli.to_owned(),
            model: model.to_owned(),
            tokens: input + output,
            input,
            output,
            cached,
            usd,
            billing,
            roles: BTreeSet::from([role.to_owned()]),
            efforts: effort.into_iter().map(str::to_owned).collect(),
        }
    }

    /// The picture's own example: a day of three models, two crews and Captain's turns.
    fn the_pictures_day() -> DailyUsage {
        let ends = |merged, abandoned| RunEnds {
            merged,
            abandoned,
            ..RunEnds::default()
        };
        DailyUsage {
            fields: 3,
            install: "7f3c09e1-5d2a-4c1b-9e8f-0a1b2c3d4e5f".to_owned(),
            version: "0.1.0".to_owned(),
            os: "macos".to_owned(),
            machine: Machine {
                chip: "apple_silicon".to_owned(),
                ram_gb: Some(64),
                cores: Some(16),
                terminal: "ghostty".to_owned(),
            },
            setup: SetupFacts {
                screen: "inline".to_owned(),
                sandbox: "os".to_owned(),
                approvals: "auto".to_owned(),
                autopilot: false,
                git_repo: true,
            },
            clis: BTreeMap::new(),
            models: vec![
                model(
                    "claude-code",
                    "claude-opus-5-5",
                    "plan",
                    "main",
                    Some("xhigh"),
                    (412_300, 38_100, 2_900_000),
                    3.12,
                ),
                model(
                    "codex",
                    "gpt-6-luna",
                    "plan",
                    "reviewer",
                    Some("low"),
                    (61_000, 4_200, 0),
                    0.08,
                ),
                model(
                    "opencode",
                    "qwen",
                    "local",
                    "worker",
                    None,
                    (88_400, 9_600, 0),
                    0.0,
                ),
            ],
            runs: BTreeMap::from([
                ("orchestrator".to_owned(), ends(2, 0)),
                ("council".to_owned(), ends(0, 1)),
                ("captain".to_owned(), ends(1, 0)),
            ]),
            turns: 3,
            tasks: Tasks {
                reviewed: 7,
                first_pass: 5,
                second_pass: 2,
                more_rounds: 0,
                never_passed: 0,
            },
            findings: Findings {
                given: 9,
                blocking: 2,
                fixed: 7,
                by_reviewer: BTreeMap::from([
                    ("gpt-6-luna".to_owned(), 6),
                    ("gemma".to_owned(), 3),
                ]),
                by_builder: BTreeMap::new(),
            },
            checks: Checks {
                tasks_checked: 6,
                first_try: 4,
                after_review: 6,
                final_check_failed: 0,
            },
            merges: Merges {
                you: 1,
                autopilot: 1,
                sent_back: 1,
                ..Merges::default()
            },
            fallbacks: Fallbacks {
                usage_limit: 1,
                to: BTreeMap::from([("gpt-6-luna".to_owned(), 1)]),
                ..Fallbacks::default()
            },
            features: Features {
                ultracode: 1,
                mods: 2,
                commands: BTreeMap::from([("/model".to_owned(), 4), ("/resume".to_owned(), 1)]),
                workflow_view: 6,
                ..Features::default()
            },
            health: Health {
                startup: BTreeMap::from([("under_1s".to_owned(), 4)]),
                ..Health::default()
            },
            behaviors: BehaviorCounts::default(),
        }
    }

    /// `argonauts telemetry show` as `telemetry_show` draws it, at its width. The copy in
    /// `tests/golden` is the picture but for three places: "Qwen series" and "Gemma series", and
    /// "usage limit" as the cause, where the picture uses two retired names, and two spaces after
    /// the arrow, which some fonts draw two cells wide.
    #[test]
    fn show_reads_as_the_picture_draws_it() {
        let theme = theme();
        let due = Due::After {
            today: true,
            clock: "14:00".to_owned(),
        };
        let lines = lines(&the_pictures_day(), &due, &[], &theme);
        crate::testing::golden::dump("telemetry_show", &lines);
        assert_eq!(drawn_rows(&lines), picture_rows(PICTURE));
        assert_looks(&theme, &lines, COLOURS);
    }

    #[test]
    fn off_held_and_due_say_so() {
        let theme = theme();
        let text = |due: &Due, held: &[Held]| {
            let lines = lines(&the_pictures_day(), due, held, &theme);
            drawn_rows(&lines).join("\n")
        };
        let off = text(&Due::NextStart, &[Held::Off, Held::NoKey]);
        assert!(off.starts_with("Usage reports are off"), "{off}");
        assert!(off.contains("no PostHog key"), "{off}");
        let debug = text(&Due::NextStart, &[Held::DebugBuild]);
        assert!(
            debug.contains("This is a debug build, so it sends nothing."),
            "{debug}"
        );
        let ci = text(&Due::NextStart, &[Held::Ci]);
        assert!(ci.starts_with("Usage reports are on. Today's report goes out at the next start"));
        assert!(ci.contains("CI is set"), "{ci}");
        let later = text(
            &Due::After {
                today: false,
                clock: "09:30".to_owned(),
            },
            &[],
        );
        assert!(later.contains("Tomorrow's report goes out at the first start after 09:30"));
    }

    /// The behaviors the report holds get a row of their own under the rest, with each model's
    /// share; a report that holds none, as the picture's day, has no such row.
    #[test]
    fn the_behaviors_show_once_the_report_counts_any() {
        let theme = theme();
        let labels = |role: &'static str, counts: &[(&str, u32)]| {
            let counts = counts.iter().map(|(label, n)| ((*label).to_owned(), *n));
            BTreeMap::from([(role, counts.collect::<BTreeMap<_, _>>())])
        };
        let mut day = the_pictures_day();
        day.behaviors = BehaviorCounts {
            behaviors: BTreeMap::from([
                (
                    "claude-opus-5-5".to_owned(),
                    labels("main", &[("read_code", 40), ("edit_code", 20)]),
                ),
                ("qwen".to_owned(), labels("worker", &[("run_tests", 12)])),
            ]),
            behavior_outcomes: BTreeMap::new(),
            behaviors_unlabelled: BTreeMap::from([("qwen".to_owned(), 3)]),
        };
        let shown = drawn_rows(&lines(&day, &Due::NextStart, &[], &theme));
        let health = shown.iter().position(|row| row.starts_with("  health"));
        assert_eq!(
            shown[health.unwrap() + 1].trim_end(),
            "  behaviors  72 actions in 3 behaviors · 3 unlabelled     \
             by model: Opus 5.5 60, Qwen series 15"
        );
        let plain = drawn_rows(&lines(&the_pictures_day(), &Due::NextStart, &[], &theme));
        assert!(!plain.iter().any(|row| row.starts_with("  behaviors")));
    }

    /// The CLIs the report sends, each with its version, get a row under the setup; the
    /// picture's day holds none, so it has no such row.
    #[test]
    fn the_clis_show_with_their_versions() {
        let theme = theme();
        let mut day = the_pictures_day();
        day.clis = BTreeMap::from([
            ("claude-code".to_owned(), "2.1.295".to_owned()),
            ("codex".to_owned(), "0.130.0".to_owned()),
        ]);
        let shown = drawn_rows(&lines(&day, &Due::NextStart, &[], &theme));
        let setup = shown.iter().position(|row| row.starts_with("  setup"));
        assert_eq!(
            shown[setup.unwrap() + 1].trim_end(),
            "  clis       Claude Code 2.1.295 · Codex 0.130.0"
        );
        let plain = drawn_rows(&lines(&the_pictures_day(), &Due::NextStart, &[], &theme));
        assert!(!plain.iter().any(|row| row.starts_with("  clis")));
    }
}
