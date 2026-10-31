//! `/bottom-line`'s changes: which CLIs show their plan meters under the input, and in what
//! order. Each change saves at once where your changes go, or where the command's flag says, and
//! prints one line naming what the line shows now and where that was saved. The first change
//! writes the CLIs out as a list, so the line never shifts after you choose.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use anyhow::Context as _;
use argonauts_core::NoteLevel;
use argonauts_core::input::LineChange;
use argonauts_core::look::display_names::builtin_cli_name;
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::bottom_line::{
    BottomLineSettings, LineCli, PlanUsageLine, PlanUsageShow,
};
use argonauts_core::settings::mods::builtin::{BOTTOM_LINE, builtin};
use time::OffsetDateTime;

use super::{header_agent, line_facts};
use crate::controller::{Ctx, Effect};
use crate::startup::env::AppEnv;

pub(crate) fn command(
    ctx: &Ctx,
    change: LineChange,
    scope: Option<SettingScope>,
) -> anyhow::Result<Vec<Effect>> {
    let place = Place::of(ctx, scope);
    let main = header_agent(ctx).0.agent;
    let crew = crew(ctx, &main);
    let known_clis = known_clis(&crew);
    let now = OffsetDateTime::now_utc();
    let before = line_facts(&ctx.env, now);
    let now_shown = shown(&before.settings, &before.most_used, &main, &crew);
    let new = match change {
        // The screen opens the picker itself.
        LineChange::Open => return Ok(Vec::new()),
        LineChange::Reset => Line::Reset,
        LineChange::Set { clis, holding_up } => Line::Listed {
            clis,
            holding_up: Some(holding_up),
        },
        LineChange::Add { cli } => {
            let Some(id) = known(&cli, &known_clis) else {
                return refuse(ctx, unknown(&cli, &known_clis));
            };
            if now_shown.iter().any(|line_cli| line_cli.cli == id) {
                return refuse(ctx, format!("The bottom line already shows {}", name(&id)));
            }
            let mut clis = now_shown;
            clis.push(LineCli::new(&id));
            Line::Listed {
                clis,
                holding_up: None,
            }
        }
        LineChange::Remove { cli } => {
            let Some(id) = known(&cli, &known_clis) else {
                return refuse(ctx, unknown(&cli, &known_clis));
            };
            if id == main {
                return refuse(
                    ctx,
                    format!(
                        "{} is the one in use, which the bottom line always shows",
                        name(&id)
                    ),
                );
            }
            if !now_shown.iter().any(|line_cli| line_cli.cli == id) {
                return refuse(ctx, format!("The bottom line doesn't show {}", name(&id)));
            }
            let clis = now_shown
                .into_iter()
                .filter(|line_cli| line_cli.cli != id)
                .collect();
            Line::Listed {
                clis,
                holding_up: None,
            }
        }
    };
    let saved = place.save(&ctx.env, &new)?;
    let after = line_facts(&ctx.env, now);
    let shows = match (&new, after.settings.plan_usage.clis.is_none()) {
        (Line::Reset, true) => "the one in use and the most used".to_owned(),
        _ => names(&shown(&after.settings, &after.most_used, &main, &crew)),
    };
    let mut said = format!("The bottom line shows {shows} · {saved}");
    // A change for this session only says what comes back after it.
    if place == Place::Session {
        let kept = super::line_settings(&ctx.env);
        let was = names(&shown(&kept, &after.most_used, &main, &crew));
        if was != shows {
            said.push_str(&format!(" · was {was}"));
        }
    }
    ctx.notice(NoteLevel::Info, said);
    Ok(vec![Effect::Status])
}

/// Nothing changes, and the line says why.
fn refuse(ctx: &Ctx, why: String) -> anyhow::Result<Vec<Effect>> {
    ctx.notice(NoteLevel::Info, why);
    Ok(Vec::new())
}

/// The line's CLIs as a change leaves them.
#[derive(Debug, Clone, PartialEq)]
enum Line {
    /// The CLIs in order, and whether one holding up a task joins them, where that changed.
    Listed {
        clis: Vec<LineCli>,
        holding_up: Option<bool>,
    },
    /// No list: the one in use and the most used.
    Reset,
}

/// Where a change to the line goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// `~/.argonauts/mods/bottom-line.toml`.
    Everywhere,
    /// The repository's `.argonauts/mods/bottom-line.toml`.
    Project,
    /// Only until you quit.
    Session,
}

impl Place {
    /// The flag's place, else where your changes go. The line has no file for you alone in a
    /// project, so that goes into yours.
    fn of(ctx: &Ctx, scope: Option<SettingScope>) -> Place {
        let usual = || crate::controller::settings::save::save_changes_to(ctx).scope();
        match scope.unwrap_or_else(usual) {
            SettingScope::Session => Place::Session,
            SettingScope::Project => Place::Project,
            SettingScope::Global | SettingScope::ProjectLocal => Place::Everywhere,
        }
    }

    /// Saves `line` here, and says where, as the line printed after it does.
    fn save(self, env: &AppEnv, line: &Line) -> anyhow::Result<String> {
        let file = match self {
            Place::Session => {
                session::keep(env, line);
                return Ok("this session".to_owned());
            }
            Place::Everywhere => BottomLineSettings::global_file(env.paths.home()),
            Place::Project => BottomLineSettings::project_file(&env.repo_root),
        };
        write(&file, line)?;
        // The change builds on the line as you see it, so a session's own change is in it.
        session::forget(env);
        Ok(match self {
            Place::Project => format!(
                "this project ({})",
                file.strip_prefix(&env.repo_root).unwrap_or(&file).display()
            ),
            _ => format!("everywhere ({})", env.display(&file)),
        })
    }
}

/// Writes `line` into `file`, keeping the rest of it as it was written. A file that isn't there
/// yet starts as the line ships, comments and all.
fn write(file: &Path, line: &Line) -> anyhow::Result<()> {
    let dir = file.parent().context("the line's file has no folder")?;
    match line {
        Line::Listed { clis, holding_up } => {
            let mut pairs = vec![("plan-usage.clis".to_owned(), clis_text(clis))];
            if let Some(holding_up) = holding_up {
                pairs.push(("plan-usage.holding-up".to_owned(), holding_up.to_string()));
            }
            let start = builtin(BOTTOM_LINE).map_or("", |line| line.text);
            crate::mods::files::save(dir, BOTTOM_LINE, start, &pairs)?;
        }
        Line::Reset => {
            let Ok(text) = std::fs::read_to_string(file) else {
                return Ok(());
            };
            let mut doc: toml_edit::DocumentMut = text
                .parse()
                .with_context(|| format!("{} isn't valid TOML", file.display()))?;
            if let Some(plan_usage) = doc
                .get_mut("plan-usage")
                .and_then(|item| item.as_table_like_mut())
            {
                plan_usage.remove("clis");
                if plan_usage.get("show").is_some() {
                    plan_usage.insert(
                        "show",
                        toml_edit::value(show_word(PlanUsageShow::default())),
                    );
                }
            }
            crate::mods::files::write(file, &doc.to_string())?;
        }
    }
    Ok(())
}

/// `[{ cli = "claude-code" }, { cli = "codex", windows = "both" }]`.
fn clis_text(clis: &[LineCli]) -> String {
    let tables: Vec<String> = clis
        .iter()
        .map(|line_cli| {
            let cli = toml_edit::Value::from(line_cli.cli.as_str());
            match line_cli.windows {
                Some(windows) => format!(
                    "{{ cli = {cli}, windows = {} }}",
                    toml_edit::Value::from(windows_word(windows))
                ),
                None => format!("{{ cli = {cli} }}"),
            }
        })
        .collect();
    format!("[{}]", tables.join(", "))
}

fn windows_word(windows: argonauts_core::settings::bottom_line::PlanUsageWindows) -> String {
    serde_json::to_value(windows)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn show_word(show: PlanUsageShow) -> String {
    serde_json::to_value(show)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The CLIs the line shows by `settings`, the one in use first, each as the list would save
/// it. A CLI that joins only while it holds up a task isn't among them.
fn shown(
    settings: &BottomLineSettings,
    most_used: &[String],
    main: &str,
    crew: &[String],
) -> Vec<LineCli> {
    let set = &settings.plan_usage;
    let mut shown = vec![LineCli::new(main)];
    let others: Vec<LineCli> = match &set.clis {
        Some(listed) => listed.clone(),
        None => match set.show {
            PlanUsageShow::All => crew.iter().map(|cli| LineCli::new(cli)).collect(),
            PlanUsageShow::Main | PlanUsageShow::NearLimit => Vec::new(),
            PlanUsageShow::MainAndMostUsed => most_used
                .iter()
                .find(|cli| *cli != main && crew.contains(cli))
                .map(|cli| LineCli::new(cli))
                .into_iter()
                .collect(),
        },
    };
    for line_cli in others {
        match shown.iter_mut().find(|known| known.cli == line_cli.cli) {
            // The one in use keeps the windows the list gives it.
            Some(known) => known.windows = line_cli.windows,
            None => shown.push(line_cli),
        }
    }
    shown
}

/// `Claude Code, Codex`.
fn names(clis: &[LineCli]) -> String {
    clis.iter()
        .map(|line_cli| name(&line_cli.cli))
        .collect::<Vec<_>>()
        .join(", ")
}

fn name(id: &str) -> String {
    builtin_cli_name(id).unwrap_or(id).to_owned()
}

/// The crew's CLIs by id, the one in use first.
fn crew(ctx: &Ctx, main: &str) -> Vec<String> {
    let mut ids = vec![main.to_owned()];
    for member in ctx.settings().crew.members() {
        if !ids.contains(&member.agent) {
            ids.push(member.agent);
        }
    }
    ids
}

/// The CLIs the line can show: the crew's, and every CLI Argonauts Code drives.
fn known_clis(crew: &[String]) -> Vec<String> {
    let mut ids = crew.to_vec();
    for adapter in crate::agents::adapters::builtin_adapters() {
        if !ids.iter().any(|id| id == adapter.id()) {
            ids.push(adapter.id().to_owned());
        }
    }
    ids
}

/// The CLI `word` names, by its id or its name in any case: `agy`, `codex`, `Claude Code`.
fn known(word: &str, clis: &[String]) -> Option<String> {
    let word = word.trim().to_lowercase();
    clis.iter()
        .find(|id| {
            **id == word
                || name(id).to_lowercase() == word
                || (word == "claude" && id.as_str() == "claude-code")
        })
        .cloned()
}

fn unknown(word: &str, clis: &[String]) -> String {
    let names: Vec<String> = clis.iter().map(|id| name(id)).collect();
    format!(
        "There's no CLI named {}; the bottom line can show {}",
        word.trim(),
        names.join(", ")
    )
}

/// What `--session` changed, laid over the files until you quit.
mod session {
    use super::*;

    /// Each app's change for its session, by the folder its settings live in, which is one per
    /// app and one per test.
    static KEPT: Mutex<Vec<(PathBuf, Line)>> = Mutex::new(Vec::new());

    pub(super) fn keep(env: &AppEnv, line: &Line) {
        let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
        let home = env.paths.home().to_path_buf();
        kept.retain(|(known, _)| *known != home);
        kept.push((home, line.clone()));
    }

    pub(super) fn forget(env: &AppEnv) {
        let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
        kept.retain(|(known, _)| known != env.paths.home());
    }

    /// `plan_usage` with this session's change laid over it.
    pub(in crate::controller::header_and_bottom_line) fn lay_over(
        env: &AppEnv,
        plan_usage: &mut PlanUsageLine,
    ) {
        let kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
        let home = env.paths.home();
        let Some((_, line)) = kept.iter().find(|(known, _)| known == home) else {
            return;
        };
        match line {
            Line::Listed { clis, holding_up } => {
                plan_usage.clis = Some(clis.clone());
                if let Some(holding_up) = holding_up {
                    plan_usage.holding_up = *holding_up;
                }
            }
            Line::Reset => {
                plan_usage.clis = None;
                plan_usage.show = PlanUsageShow::default();
            }
        }
    }
}

pub(super) use session::lay_over;

#[cfg(test)]
mod tests;
