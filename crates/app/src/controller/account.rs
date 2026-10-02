//! You and your agents' accounts, whatever CLI the main agent is: `/usage` shows quotas and
//! spend, `/login` and `/logout` hand the terminal to an agent CLI's own login, and
//! `/release-notes` and `/feedback` are about Argonauts itself.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;

use anyhow::{Context as _, bail};
use argonauts_core::app::{AppEvent, QuotaChip};
use argonauts_core::limits::{HealthState, QuotaStatus};
use argonauts_core::{NoteLevel, Spend};
use time::{Duration, OffsetDateTime};

use super::status_line::quota_chips;
use super::{Ctx, Effect};

const FEEDBACK_FILE: &str = "feedback.jsonl";

/// What's new, newest release first. Each entry's first line is its heading.
const RELEASE_NOTES: &[&[&str]] = &[&[
    "Argonauts 0.0.1",
    "Runs Claude Code, Codex, agy, OpenCode, and model endpoints as one crew, in a screen that \
     works like Claude Code's.",
    "Chat with the main agent; /solo and /crew hand work to one agent or the crew.",
    "The commands the agent CLIs share work whatever CLI the main agent is: /compact, /rename, \
     /branch, /fork, /btw, /recap, /copy, /export, /cd, /add-dir, /plan, /goal, /fast, \
     /output-style, /diff, /review, /security-review, /usage, /login, and /logout.",
    "Reviews go to an agent from another company than the main agent's when the crew has one.",
]];

/// The span `/usage` adds runs up over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Period {
    Day,
    Week,
    All,
}

impl Period {
    fn parse(word: Option<&str>) -> anyhow::Result<Self> {
        Ok(match word.map(str::to_ascii_lowercase).as_deref() {
            None | Some("day" | "daily" | "today") => Period::Day,
            Some("week" | "weekly") => Period::Week,
            Some("all" | "cumulative" | "total") => Period::All,
            Some(other) => bail!("/usage takes day, week, or all, not {other:?}."),
        })
    }

    fn title(self) -> &'static str {
        match self {
            Period::Day => "Usage, last 24 hours",
            Period::Week => "Usage, last 7 days",
            Period::All => "Usage, all time",
        }
    }

    fn since(self, now: OffsetDateTime) -> Option<OffsetDateTime> {
        match self {
            Period::Day => Some(now - Duration::days(1)),
            Period::Week => Some(now - Duration::days(7)),
            Period::All => None,
        }
    }
}

/// `/usage [day|week|all]`: each agent's quota now, then what the conversation and this repo's
/// runs spent. Reads only what's recorded, so it costs nothing.
pub(crate) async fn usage(
    ctx: &mut Ctx,
    conversation: Option<Spend>,
    period: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let period = Period::parse(period.as_deref())?;
    let now = OffsetDateTime::now_utc();
    let settings = ctx.settings();
    let mut crew = vec![settings.default_agent.clone()];
    crew.extend(settings.crew.iter().cloned());
    let chips = quota_chips(
        &crew,
        ctx.services.quota.as_ref(),
        ctx.services.health.as_ref(),
    );
    let mut lines: Vec<String> = chips.iter().map(|chip| quota_line(chip, now)).collect();
    if let Some(spent) = conversation {
        lines.push(format!("This conversation · {}", usd(&spent)));
    }
    lines.push(match ctx.services.conductor.list().await {
        Ok(runs) => {
            let since = period.since(now);
            let counted: Vec<_> = runs
                .iter()
                .filter(|run| since.is_none_or(|since| run.updated >= since))
                .collect();
            let total = counted
                .iter()
                .fold(Spend::default(), |sum, run| sum.plus(&run.spent));
            let runs = match counted.len() {
                1 => "1 run".to_owned(),
                n => format!("{n} runs"),
            };
            format!("Runs in this repo · {runs} · {}", usd(&total))
        }
        Err(err) => format!("Runs in this repo · couldn't list them: {err}"),
    });
    ctx.send(AppEvent::Report {
        title: period.title().to_owned(),
        lines,
    });
    Ok(Vec::new())
}

/// `Claude · 42% of the five_hour window · resets in 2h 10m`.
fn quota_line(chip: &QuotaChip, now: OffsetDateTime) -> String {
    let mut parts = vec![chip.label.clone()];
    if chip.health == HealthState::Down {
        parts.push("down".to_owned());
    }
    let Some(reading) = &chip.reading else {
        parts.push("no quota reading yet".to_owned());
        return parts.join(" · ");
    };
    let window = reading
        .window
        .as_deref()
        .map(|window| window.replace('_', "-"))
        .unwrap_or_else(|| "current".to_owned());
    match (reading.status, reading.used) {
        (QuotaStatus::Rejected, _) => parts.push(format!("out of the {window} window")),
        (_, Some(used)) => parts.push(format!(
            "{:.0}% of the {window} window used",
            (used * 100.0).clamp(0.0, 100.0)
        )),
        (QuotaStatus::Warning, None) => parts.push(format!("near the {window} limit")),
        (QuotaStatus::Allowed | QuotaStatus::Unknown, None) => {
            parts.push(format!("{window} window open"));
        }
    }
    if let Some(resets) = reading.resets_at.filter(|resets| *resets > now) {
        parts.push(format!("resets in {}", span(resets - now)));
    }
    parts.join(" · ")
}

fn span(left: Duration) -> String {
    let minutes = left.whole_minutes().max(1);
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) if h % 24 == 0 => format!("{}d", h / 24),
        (h, 0) => format!("{h}h"),
        (h, m) if h < 24 => format!("{h}h {m}m"),
        (h, _) => format!("{}d {}h", h / 24, h % 24),
    }
}

fn usd(spend: &Spend) -> String {
    argonauts_tui::format::spend(spend)
}

/// `/login` and `/logout`: the screen steps aside while the agent CLI's own command runs in
/// the terminal, as when you take over a card. No agent means the main agent's CLI.
pub(crate) fn login(
    ctx: &mut Ctx,
    agent: Option<String>,
    log_in: bool,
) -> anyhow::Result<Vec<Effect>> {
    let command = if log_in { "/login" } else { "/logout" };
    let agent = agent.unwrap_or_else(|| ctx.settings().default_agent.worker.clone());
    let adapters = crate::commands::adapters_turning_on(
        Some(&ctx.env),
        ctx.settings.resolved(),
        std::slice::from_ref(&agent),
    )?;
    let Some(adapter) = adapters.iter().find(|adapter| adapter.id() == agent) else {
        bail!("{agent} isn't an agent CLI Argonauts knows. /agents lists them.");
    };
    let Some(login) = adapter.setup_hints().login else {
        bail!(
            "{agent} has no login of its own; its keys live in your settings. /agents shows them."
        );
    };
    let mut argv: Vec<String> = login.split_whitespace().map(str::to_owned).collect();
    if !log_in {
        argv = match logout_of(&argv) {
            Some(argv) => argv,
            None => bail!(
                "{agent} logs out from its own screen: run `{}` and log out there.",
                argv.join(" ")
            ),
        };
    }
    if let Some(binary) = ctx
        .settings()
        .agents
        .get(&agent)
        .and_then(|settings| settings.binary.as_ref())
        && let Some(program) = argv.first_mut()
    {
        *program = binary.display().to_string();
    }
    let shown = argv.join(" ");
    if !ctx.env.interactive {
        bail!(
            "{command} hands the terminal to `{shown}`, and there's no terminal here. Run it in one."
        );
    }
    ctx.notice(
        NoteLevel::Info,
        format!("Running `{shown}`. The screen comes back when it ends."),
    );
    ctx.send(AppEvent::RunInTerminal {
        argv,
        cwd: ctx.env.repo_root.clone(),
    });
    Ok(Vec::new())
}

/// The CLIs pair them by name: `codex login` and `codex logout`, `claude auth login` and
/// `claude auth logout`. A CLI that logs in with no such word has no logout command.
fn logout_of(login: &[String]) -> Option<Vec<String>> {
    let (last, rest) = login.split_last()?;
    (last == "login").then(|| {
        let mut argv = rest.to_vec();
        argv.push("logout".to_owned());
        argv
    })
}

/// `/release-notes`.
pub(crate) fn release_notes(ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
    let mut lines = vec![format!("This is Argonauts {}.", env!("CARGO_PKG_VERSION"))];
    for release in RELEASE_NOTES {
        let Some((heading, notes)) = release.split_first() else {
            continue;
        };
        lines.push(String::new());
        lines.push((*heading).to_owned());
        lines.extend(notes.iter().map(|note| format!("- {note}")));
    }
    ctx.send(AppEvent::Report {
        title: "Release notes".to_owned(),
        lines,
    });
    Ok(Vec::new())
}

/// `/feedback <text>`: kept in `~/.argonauts/feedback.jsonl`. Nothing leaves this machine, so
/// you share that file when you report something.
pub(crate) fn feedback(ctx: &mut Ctx, text: Option<String>) -> anyhow::Result<Vec<Effect>> {
    let text = text.unwrap_or_default().trim().to_owned();
    if text.is_empty() {
        ctx.notice(
            NoteLevel::Info,
            "Put your feedback after /feedback, such as /feedback the menu is slow.",
        );
        return Ok(Vec::new());
    }
    let path = ctx.env.paths.home().join(FEEDBACK_FILE);
    let at = OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let entry = serde_json::json!({
        "at": at,
        "version": env!("CARGO_PKG_VERSION"),
        "text": text,
    });
    let mut line = serde_json::to_vec(&entry).context("encoding the feedback")?;
    line.push(b'\n');
    let shown = ctx.env.display(&path);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(&line))
        .with_context(|| format!("saving your feedback to {shown}"))?;
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Thanks. Saved to {shown}. Nothing leaves this machine, so share that file when you \
             report something."
        ),
    );
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::{QuotaReading, QuotaSource};
    use argonauts_core::names::VendorId;

    fn chip(status: QuotaStatus, used: Option<f64>, resets_in: Option<Duration>) -> QuotaChip {
        let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        QuotaChip {
            label: "Codex".to_owned(),
            reading: Some(QuotaReading {
                vendor: VendorId("codex".to_owned()),
                status,
                used,
                resets_at: resets_in.map(|left| now + left),
                window: Some("five_hour".to_owned()),
                source: QuotaSource::AppServer,
                observed_at: now,
                detail: None,
            }),
            health: HealthState::Healthy,
        }
    }

    #[test]
    fn a_quota_line_says_how_much_is_used_and_when_it_resets() {
        let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        let line = quota_line(
            &chip(
                QuotaStatus::Allowed,
                Some(0.42),
                Some(Duration::minutes(130)),
            ),
            now,
        );
        assert_eq!(
            line,
            "Codex · 42% of the five-hour window used · resets in 2h 10m"
        );
        let out = quota_line(&chip(QuotaStatus::Rejected, Some(1.0), None), now);
        assert_eq!(out, "Codex · out of the five-hour window");
        let none = QuotaChip {
            reading: None,
            health: HealthState::Down,
            ..chip(QuotaStatus::Unknown, None, None)
        };
        assert_eq!(
            quota_line(&none, now),
            "Codex · down · no quota reading yet"
        );
    }

    #[test]
    fn periods_take_the_vendors_words_too() {
        assert_eq!(Period::parse(None).unwrap(), Period::Day);
        assert_eq!(Period::parse(Some("weekly")).unwrap(), Period::Week);
        assert_eq!(Period::parse(Some("cumulative")).unwrap(), Period::All);
        assert!(Period::parse(Some("monthly")).is_err());
    }

    #[test]
    fn logout_pairs_with_login_by_name() {
        let argv =
            |text: &str| -> Vec<String> { text.split_whitespace().map(str::to_owned).collect() };
        assert_eq!(logout_of(&argv("codex login")), Some(argv("codex logout")));
        assert_eq!(
            logout_of(&argv("claude auth login")),
            Some(argv("claude auth logout"))
        );
        assert_eq!(logout_of(&argv("agy")), None);
    }

    #[test]
    fn spend_says_when_it_is_a_floor_or_a_guess() {
        let spend = Spend {
            cost_usd: 1.5,
            cost_partial: true,
            ..Spend::default()
        };
        assert_eq!(usd(&spend), "$1.50+");
        let estimated = Spend {
            cost_usd: 0.24,
            cost_estimated: true,
            ..Spend::default()
        };
        assert_eq!(usd(&estimated), "~$0.24");
        assert_eq!(span(Duration::hours(48)), "2d");
        assert_eq!(span(Duration::seconds(10)), "1m");
    }
}
