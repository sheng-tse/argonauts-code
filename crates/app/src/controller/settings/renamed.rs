//! A settings file that still uses names that changed gets one question at start, with the
//! exact edit: rename them, keeping a copy of the file, or leave them and be asked again only
//! once the file changes. Until then each old name does nothing, and /doctor lists them.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use argonauts_core::app::{AppChoice, AppEvent, ChoiceKind};
use argonauts_core::events::approval::AnswerAction;
use argonauts_core::names::ChoiceId;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_registry::{Rename, rename_in_file, renames_in};

use super::save;
use crate::choices;
use crate::controller::{Ctx, Effect};
use crate::trust::hash_of;

type Handled = anyhow::Result<Vec<Effect>>;

/// Where the files you chose to leave are kept, each with its hash as you left it.
const LEFT_FILE: &str = "renames-left.json";

#[derive(Default)]
pub(crate) struct RenamedState {
    pending: HashMap<ChoiceId, Asked>,
}

struct Asked {
    file: PathBuf,
    /// The file as you were asked about it.
    hash: String,
}

pub(crate) fn owns_choice(state: &RenamedState, id: &ChoiceId) -> bool {
    state.pending.contains_key(id)
}

/// The files the start may ask about: your own, and a project file once you trust it. One that
/// waits for your trust is asked about after, so a rename never changes it under the trust
/// question.
fn files(ctx: &Ctx) -> Vec<PathBuf> {
    let mut files = vec![ctx.env.global_config()];
    for file in [ctx.env.project_config(), ctx.env.project_local_config()] {
        if ctx.settings.trust().is_trusted(&file) {
            files.push(file);
        }
    }
    files
}

/// Whether the start says this file's old names itself, by asking or because you left them,
/// so its lines among the start's warnings would say them twice.
pub(crate) fn said_at_start(ctx: &Ctx, file: &Path) -> bool {
    files(ctx).iter().any(|asked| asked == file)
}

/// At start: one question for each file that uses old names, unless you left it as it is.
pub(crate) fn ask_at_start(ctx: &mut Ctx, state: &mut RenamedState) {
    let left = left_files(ctx);
    for file in files(ctx) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let renames = renames_in(&text);
        let Some(hash) = hash_of(&file).filter(|_| !renames.is_empty()) else {
            continue;
        };
        if left.get(&key(&file)) == Some(&hash) {
            continue;
        }
        let id = ctx.next_choice_id();
        let shown = ctx.env.display(&file);
        ctx.send(AppEvent::Choice {
            choice: question(id.clone(), &shown, &renames),
        });
        state.pending.insert(id, Asked { file, hash });
    }
}

/// The question, worded from the file's own lines: each old name beside its new one, then what
/// renaming does and what leaving them does.
pub(crate) fn question(id: ChoiceId, shown: &str, renames: &[Rename]) -> AppChoice {
    let count = match renames.len() {
        1 => "1 name that changed".to_owned(),
        n => format!("{n} names that changed"),
    };
    let width = renames
        .iter()
        .map(|rename| rename.old.chars().count())
        .max()
        .unwrap_or(0)
        + 6;
    // Each row's parts are plain and dim in turn, so the empty first part makes the old name dim.
    let mut body = vec![String::new()];
    body.extend(renames.iter().map(|rename| {
        let note = rename
            .note
            .as_deref()
            .map(|note| format!("\t   ({note})"))
            .unwrap_or_default();
        format!("\t  {:<width$}\t→ {}{note}", rename.old, rename.new)
    }));
    body.push(String::new());
    body.push(
        "\t  Until they're renamed they are skipped, and the defaults are used instead.".to_owned(),
    );
    let backup = Path::new(shown)
        .file_name()
        .map(|name| format!("{}.bak", name.to_string_lossy()))
        .unwrap_or_default();
    let mut options = choices::options(ChoiceKind::RenamedSettings);
    options[0].detail = Some(format!("a copy of the file is kept as {backup}"));
    options[1].detail =
        Some("asked again only when the file changes; /doctor keeps listing them".to_owned());
    choices::app_choice(
        id,
        ChoiceKind::RenamedSettings,
        format!("{shown} uses {count}"),
        body,
        options,
    )
}

pub(crate) fn answer_choice(
    ctx: &mut Ctx,
    state: &mut RenamedState,
    id: ChoiceId,
    action: AnswerAction,
) -> Handled {
    let Some(asked) = state.pending.remove(&id) else {
        return Ok(Vec::new());
    };
    match action {
        AnswerAction::Approve => rename(ctx, id, &asked),
        _ => {
            leave(ctx, id, &asked)?;
            Ok(Vec::new())
        }
    }
}

/// Esc leaves them, as the key line says.
pub(crate) fn set_aside(ctx: &mut Ctx, state: &mut RenamedState, id: ChoiceId) -> Handled {
    if let Some(asked) = state.pending.remove(&id) {
        leave(ctx, id, &asked)?;
    }
    Ok(Vec::new())
}

fn rename(ctx: &mut Ctx, id: ChoiceId, asked: &Asked) -> Handled {
    let shown = ctx.env.display(&asked.file);
    if hash_of(&asked.file).as_deref() != Some(asked.hash.as_str()) {
        close(
            ctx,
            id,
            format!("{shown} changed since you were asked, so it is left as it is."),
        );
        return Ok(Vec::new());
    }
    let before = ctx.settings().clone();
    let values_before = values(ctx);
    let renamed = match rename_in_file(&asked.file) {
        Ok(renamed) => renamed,
        Err(err) => {
            close(ctx, id, format!("{shown} is left as it was: {err}"));
            return Ok(Vec::new());
        }
    };
    let env = ctx.env.clone();
    ctx.settings.reload(&env)?;
    let values_after = values(ctx);
    let backup = ctx.env.display(&renamed.backup);
    let mut outcome = format!(
        "Renamed {} in {shown}. The file as it was is {backup}.",
        counted(renamed.renames.len())
    );
    for rename in renamed.renames.iter().filter(|rename| rename.safety) {
        let was = values_before
            .get(&rename.to)
            .map_or("unset", String::as_str);
        let now = values_after.get(&rename.to).map_or("unset", String::as_str);
        outcome.push_str(&format!(" {} was {was}, now {now}.", rename.to));
    }
    close(ctx, id, outcome);
    if ctx.settings().ui != before.ui {
        save::send_ui(ctx);
    }
    let mut effects = Vec::new();
    if ctx.settings().crew.main != before.crew.main {
        effects.push(Effect::FreshConversation);
    }
    let keys = renamed
        .renames
        .into_iter()
        .map(|rename| rename.to)
        .collect();
    effects.push(Effect::SettingsApplied { keys });
    Ok(effects)
}

/// Each setting's value in effect now, by key.
fn values(ctx: &Ctx) -> BTreeMap<String, String> {
    ctx.settings
        .resolved()
        .entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect()
}

fn leave(ctx: &Ctx, id: ChoiceId, asked: &Asked) -> anyhow::Result<()> {
    let mut left = left_files(ctx);
    left.insert(key(&asked.file), asked.hash.clone());
    let path = left_path(ctx);
    if let Some(dir) = path.parent() {
        create_private_dir_all(dir)?;
    }
    write_private_file(&path, serde_json::to_string_pretty(&left)?.as_bytes())?;
    let shown = ctx.env.display(&asked.file);
    close(
        ctx,
        id,
        format!(
            "Left them. You're asked again when {shown} changes, and /doctor keeps listing them."
        ),
    );
    Ok(())
}

fn left_path(ctx: &Ctx) -> PathBuf {
    ctx.env.paths.home().join("state").join(LEFT_FILE)
}

fn left_files(ctx: &Ctx) -> BTreeMap<String, String> {
    std::fs::read_to_string(left_path(ctx))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn key(file: &Path) -> String {
    file.display().to_string()
}

fn counted(count: usize) -> String {
    match count {
        1 => "1 name".to_owned(),
        n => format!("{n} names"),
    }
}

fn close(ctx: &Ctx, id: ChoiceId, outcome: impl Into<String>) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.into(),
    });
}

#[cfg(test)]
mod tests {
    use argonauts_registry::WarningAbout;
    use argonauts_testkit::TempRepo;
    use argonauts_tui::questions::choice_question;

    use super::*;
    use crate::controller::test_support::{CliOptions, TestCtx};
    use crate::golden::{assert_looks, drawn_rows, picture_rows, still, theme};

    /// A global file an older build wrote, with three names that changed since.
    const OLDER: &str = r#"main_agent = "claude-code"
ui.keys = "vim"
workflow = "solo"
crew = ["claude-code", "codex", "antigravity-cli"]

[approvals]
autopilot = false

[approvals.goal]
mode = "supervised"
"#;

    fn older_home() -> TestCtx {
        let (parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(repo.home().join("config.toml"), OLDER).unwrap();
            })),
            ..CliOptions::default()
        });
        parts
    }

    fn asked(parts: &mut TestCtx) -> Option<AppChoice> {
        while let Ok(event) = parts.events.try_recv() {
            if let AppEvent::Choice { choice } = event {
                return Some(choice);
            }
        }
        None
    }

    fn closed(parts: &mut TestCtx) -> String {
        while let Ok(event) = parts.events.try_recv() {
            if let AppEvent::ChoiceClosed { outcome, .. } = event {
                return outcome;
            }
        }
        panic!("the question stayed open");
    }

    #[tokio::test]
    async fn renaming_writes_the_new_names_and_keeps_a_copy() {
        let mut parts = older_home();
        let file = parts.ctx.env.global_config();
        let shown = parts.ctx.env.display(&file);
        let mut state = RenamedState::default();
        ask_at_start(&mut parts.ctx, &mut state);
        let choice = asked(&mut parts).expect("the rename question");
        assert_eq!(choice.title, format!("{shown} uses 3 names that changed"));
        let effects =
            answer_choice(&mut parts.ctx, &mut state, choice.id, AnswerAction::Approve).unwrap();
        let outcome = closed(&mut parts);
        assert!(
            outcome.starts_with(&format!(
                "Renamed 3 names in {shown}. The file as it was is {shown}.bak."
            )),
            "{outcome}"
        );
        assert!(
            outcome.ends_with("approvals.understand.mode was \"auto\", now \"supervised\"."),
            "{outcome}"
        );
        assert!(renames_in(&std::fs::read_to_string(&file).unwrap()).is_empty());
        let backup = file.with_file_name("config.toml.bak");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), OLDER);
        assert_eq!(parts.ctx.settings().workflow, "captain");
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::SettingsApplied { .. }))
        );
    }

    #[tokio::test]
    async fn leaving_them_is_quiet_until_the_file_changes() {
        let mut parts = older_home();
        let file = parts.ctx.env.global_config();
        let mut state = RenamedState::default();
        ask_at_start(&mut parts.ctx, &mut state);
        let choice = asked(&mut parts).expect("the rename question");
        set_aside(&mut parts.ctx, &mut state, choice.id).unwrap();
        assert!(closed(&mut parts).starts_with("Left them."));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), OLDER);

        ask_at_start(&mut parts.ctx, &mut state);
        assert!(asked(&mut parts).is_none());
        std::fs::write(&file, format!("{OLDER}# changed\n")).unwrap();
        ask_at_start(&mut parts.ctx, &mut state);
        assert!(asked(&mut parts).is_some());
    }

    #[tokio::test]
    async fn the_start_leaves_their_lines_to_the_question_and_doctor_lists_them() {
        let parts = older_home();
        let warnings = &parts.ctx.settings.resolved().warnings;
        let renamed: Vec<&str> = warnings
            .iter()
            .filter(|warning| matches!(warning.about, WarningAbout::Renamed { .. }))
            .map(|warning| warning.text.as_str())
            .collect();
        assert_eq!(renamed.len(), 3, "{warnings:#?}");
        assert!(
            renamed
                .iter()
                .any(|text| text.starts_with("main_agent in ") && text.contains("now [crew] main")),
            "{renamed:?}"
        );
        assert!(
            renamed
                .iter()
                .any(|text| text.starts_with("workflow = ") && text.contains("names no workflow")),
            "{renamed:?}"
        );
        let said = crate::startup::start_notices(&parts.ctx);
        assert!(
            !said
                .iter()
                .any(|(_, text)| text.contains("main_agent") || text.contains("approvals.goal")),
            "{said:#?}"
        );
    }

    #[test]
    fn the_rename_question_reads_as_the_picture_draws_it() {
        let choice = question(
            ChoiceId::numbered(1),
            "~/.argonauts/config.toml",
            &renames_in(OLDER),
        );
        let theme = theme();
        let lines = choice_question(1, &choice).lines(&still(&theme));
        crate::golden::dump("start_after", &lines);
        assert_eq!(
            drawn_rows(&lines),
            picture_rows(include_str!("../../../tests/golden/start_after.txt"))
        );
        assert_looks(
            &theme,
            &lines,
            include_str!("../../../tests/golden/start_after.html"),
        );
    }
}
