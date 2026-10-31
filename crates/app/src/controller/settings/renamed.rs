//! A settings file that still uses names that changed gets one question at start, with the
//! exact edit: rename them, keeping a copy of the file, or leave them and be asked again only
//! once the file changes. Until then each old name does nothing, and /doctor lists them. A mod
//! file that still names a place by its old word is asked about the same way, since until then
//! the mod doesn't load.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use argonauts_core::app::{AppChoice, AppEvent, ChoiceKind};
use argonauts_core::base::names::ChoiceId;
use argonauts_core::base::paths::{create_private_dir_all, write_private_file};
use argonauts_core::runs::events::approval::AnswerAction;
use argonauts_registry::{Rename, mod_renames_in, rename_in_file, rename_in_mod_file, renames_in};

use super::save;
use crate::controller::{Ctx, Effect};
use crate::setup::choices;
use crate::startup::trust::hash_of;

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
    kind: FileKind,
}

/// What a file asked about is, which says what its old names do and how it's read again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Settings,
    Mod,
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

/// The mod files the start may ask about: each of yours, and each of the project's you trusted
/// as it is, which the edit keeps trusted.
fn mod_files(ctx: &Ctx) -> Vec<PathBuf> {
    let dirs = crate::mods::ModDirs::of(&ctx.env);
    let mut files = toml_files_in(&dirs.yours);
    files.extend(
        toml_files_in(&dirs.project)
            .into_iter()
            .filter(|file| ctx.settings.trust().was_trusted(file)),
    );
    files
}

/// The `*.toml` files right in `dir`, by name, as the mods folder loads them.
fn toml_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
        .collect();
    files.sort();
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
    let settings = files(ctx)
        .into_iter()
        .map(|file| (file, FileKind::Settings));
    let mods = mod_files(ctx).into_iter().map(|file| (file, FileKind::Mod));
    for (file, kind) in settings.chain(mods).collect::<Vec<_>>() {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let renames = match kind {
            FileKind::Settings => renames_in(&text),
            FileKind::Mod => mod_renames_in(&text),
        };
        let Some(hash) = hash_of(&file).filter(|_| !renames.is_empty()) else {
            continue;
        };
        if left.get(&key(&file)) == Some(&hash) {
            continue;
        }
        let id = ctx.next_choice_id();
        let shown = ctx.env.display(&file);
        let choice = match kind {
            FileKind::Settings => question(id.clone(), &shown, &renames),
            FileKind::Mod => mod_question(id.clone(), &shown, &renames),
        };
        ctx.send(AppEvent::Choice { choice });
        state.pending.insert(id, Asked { file, hash, kind });
    }
}

/// The question, worded from the file's own lines: two headed columns, each line as the file has
/// it and as Rename writes it, then what renaming does and what leaving them does.
pub(crate) fn question(id: ChoiceId, shown: &str, renames: &[Rename]) -> AppChoice {
    worded(
        id,
        shown,
        renames,
        "Until they're renamed they are skipped, and the defaults are used instead.",
    )
}

/// The same question about a mod file, whose old place word keeps the whole mod from loading.
fn mod_question(id: ChoiceId, shown: &str, renames: &[Rename]) -> AppChoice {
    worded(
        id,
        shown,
        renames,
        "Until it's renamed the mod doesn't load, and the rest work without it.",
    )
}

/// The question with `until`, what the old names do until they're renamed, under its lines.
fn worded(id: ChoiceId, shown: &str, renames: &[Rename], until: &str) -> AppChoice {
    const NOW: &str = "In the file now";
    const AFTER: &str = "After Rename them";
    let count = match renames.len() {
        1 => "1 name that changed".to_owned(),
        n => format!("{n} names that changed"),
    };
    // Two columns the screen lays out to its width, four tab-separated parts a row: the line
    // as the file has it, then as Rename writes it, each cell's plain words then its dim ones.
    // The headings and the notes are dim.
    let cell = |text: &str| text.replace('\t', " ");
    let mut body = vec![String::new(), format!("\t{NOW}\t\t{AFTER}")];
    for rename in renames {
        body.push(format!("{}\t\t{}\t", cell(&rename.old), cell(&rename.new)));
        if let Some(note) = &rename.note {
            body.push(format!("\t\t\t{}", cell(note)));
        }
    }
    body.push(String::new());
    body.push(format!("\t  {until}"));
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
    if asked.kind == FileKind::Mod {
        rename_mod(ctx, id, asked)?;
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

/// Renames a mod file's old place words. The mods read the file again as it changes, so
/// nothing else is reloaded, and a project file you trusted as it was stays trusted.
fn rename_mod(ctx: &mut Ctx, id: ChoiceId, asked: &Asked) -> anyhow::Result<()> {
    let shown = ctx.env.display(&asked.file);
    let trusted = ctx.settings.trust().was_trusted(&asked.file);
    let renamed = match rename_in_mod_file(&asked.file) {
        Ok(renamed) => renamed,
        Err(err) => {
            close(ctx, id, format!("{shown} is left as it was: {err}"));
            return Ok(());
        }
    };
    if trusted {
        ctx.settings.trust_mut().trust(&asked.file)?;
    }
    let backup = ctx.env.display(&renamed.backup);
    close(
        ctx,
        id,
        format!(
            "Renamed {} in {shown}. The file as it was is {backup}.",
            counted(renamed.renames.len())
        ),
    );
    Ok(())
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
    use crate::controller::base::test_support::{CliOptions, TestCtx};
    use crate::testing::golden::{assert_looks, drawn_rows, picture_rows, still, theme};

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

    /// The branch starter as an older build wrote it, under the line's old place word, with
    /// its own comments.
    fn older_branch() -> String {
        let (old, _) = argonauts_core::settings::mods::check::RENAMED_PLACES[0];
        format!(
            r#"# The branch you're on, as one line of words.
name   = "branch"
title  = "branch"
body   = "text"                # stack · meters · list · columns · sparkline · marks · text
places = ["{old}"]         # where it shows
form   = "line"                # full · compact · line

[data]
command = ["git", "branch", "--show-current"]
every   = "10s"
timeout = "2s"
"#
        )
    }

    /// An older mod file doesn't load, is asked about at start, and once renamed loads on the
    /// bottom line, its comments kept and the file as it was beside it.
    #[tokio::test]
    async fn an_older_mod_file_is_asked_about_and_renamed() {
        let (old, new) = argonauts_core::settings::mods::check::RENAMED_PLACES[0];
        let (mut parts, _) = TestCtx::new(CliOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                let mods = repo.home().join("mods");
                std::fs::create_dir_all(&mods).unwrap();
                std::fs::write(mods.join("branch.toml"), older_branch()).unwrap();
            })),
            ..CliOptions::default()
        });
        let file = crate::mods::ModDirs::of(&parts.ctx.env)
            .yours
            .join("branch.toml");
        let shown = parts.ctx.env.display(&file);
        let before = crate::mods::loaded(&parts.ctx.env);
        let why: Vec<&String> = before
            .problems
            .iter()
            .flat_map(|problem| &problem.problems)
            .collect();
        assert_eq!(why, [&format!("{old} is now {new}")]);

        let mut state = RenamedState::default();
        ask_at_start(&mut parts.ctx, &mut state);
        let choice = asked(&mut parts).expect("the rename question");
        assert_eq!(choice.title, format!("{shown} uses 1 name that changed"));
        let row = format!("places = [\"{old}\"]\t\tplaces = [\"{new}\"]\t");
        assert!(choice.body.contains(&row), "{:#?}", choice.body);
        assert!(
            choice
                .body
                .iter()
                .any(|line| line.contains("Until it's renamed the mod doesn't load")),
            "{:#?}",
            choice.body
        );
        let effects =
            answer_choice(&mut parts.ctx, &mut state, choice.id, AnswerAction::Approve).unwrap();
        assert!(
            effects.is_empty(),
            "nothing but the mods reads the file again"
        );
        assert_eq!(
            closed(&mut parts),
            format!("Renamed 1 name in {shown}. The file as it was is {shown}.bak.")
        );
        let backup = file.with_file_name("branch.toml.bak");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), older_branch());
        let now = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            now,
            older_branch().replace(old, new),
            "only the word changes"
        );

        let after = crate::mods::loaded(&parts.ctx.env);
        assert!(after.problems.is_empty(), "{:#?}", after.problems);
        let branch = after
            .mods
            .iter()
            .find(|loaded| loaded.file.name == "branch")
            .expect("the branch mod loads");
        assert!(branch.asks_for_line, "on the bottom line");
        ask_at_start(&mut parts.ctx, &mut state);
        assert!(asked(&mut parts).is_none(), "nothing is left to ask");
    }

    /// The picture's file: the same three names, with the values it shows.
    const PICTURED: &str = r#"main_agent = "claude-code"
workflow = "solo"
crew = ["claude-code", "codex", "antigravity-cli"]

[approvals.goal]
mode = "ask"
"#;

    #[test]
    fn the_rename_question_reads_as_the_picture_draws_it() {
        let choice = question(
            ChoiceId::numbered(1),
            "~/.argonauts/config.toml",
            &renames_in(PICTURED),
        );
        let theme = theme();
        let lines = choice_question(1, &choice).lines(&still(&theme));
        crate::testing::golden::dump("rename_after", &lines);
        assert_eq!(
            drawn_rows(&lines),
            picture_rows(include_str!("../../../tests/golden/rename_after.txt"))
        );
        assert_looks(
            &theme,
            &lines,
            include_str!("../../../tests/golden/rename_after.html"),
        );
    }

    /// The question's rows at `width`, without the blanks at their ends.
    fn rows_at(choice: &AppChoice, width: u16) -> Vec<String> {
        use argonauts_tui::base::draw::render::{Clock, RenderCtx};
        let theme = theme();
        let ctx = RenderCtx::new(width, &theme, Clock::still(std::time::Instant::now()));
        drawn_rows(&choice_question(1, choice).lines(&ctx))
            .into_iter()
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    /// The row that starts with `start` and the rows that carry it on, which start with blanks.
    fn carried<'a>(rows: &'a [String], start: &str) -> Vec<&'a str> {
        let at = rows
            .iter()
            .position(|row| row.starts_with(start))
            .unwrap_or_else(|| panic!("no row starts {start:?}: {rows:#?}"));
        std::iter::once(rows[at].as_str())
            .chain(
                rows[at + 1..]
                    .iter()
                    .map(String::as_str)
                    .take_while(|row| row.starts_with("     ") && !row.trim().is_empty()),
            )
            .collect()
    }

    /// Where the columns have room, each value wraps in its own column; where the right one
    /// hasn't, each line as the file has it and as Rename writes it takes rows of its own,
    /// each started with its column's name.
    #[test]
    fn the_rename_question_lays_its_columns_out_to_the_screen() {
        const LONG: &str = r#"main_agent = "claude-code/claude-opus-5-5-with-a-very-long-model-name"
workflow = "solo"

[approvals.goal]
mode = "ask"
"#;
        let long = question(
            ChoiceId::numbered(1),
            "~/.argonauts/config.toml",
            &renames_in(LONG),
        );
        let old = r#"main_agent = "claude-code/claude-opus-5-5-with-a-very-long-model-name""#;
        let right_at = 3 + old.len() + 6;

        let wide = rows_at(&long, 140);
        let heading = carried(&wide, "   In the file now");
        assert_eq!(
            heading[0].find("After Rename them"),
            Some(right_at),
            "{wide:#?}"
        );
        let pair = carried(&wide, &format!("   {old}"));
        assert!(pair.len() > 1, "the long value wraps: {wide:#?}");
        assert_eq!(pair[0].find("[crew] main ="), Some(right_at), "{wide:#?}");
        for row in &pair[1..] {
            let words = row.len() - row.trim_start().len();
            assert_eq!(
                words, right_at,
                "a wrapped value stays in its column: {wide:#?}"
            );
        }

        for width in [100, 80] {
            let narrow = rows_at(&long, width);
            assert!(
                !narrow.iter().any(|row| row.contains("After Rename them  ")
                    || row.starts_with("   In the file now  ")),
                "no column headings at {width}: {narrow:#?}"
            );
            let now = carried(&narrow, "   In the file now: main_agent =");
            let after = carried(&narrow, "   After Rename them: [crew] main =");
            for row in now[1..].iter().chain(&after[1..]) {
                assert!(row.len() <= usize::from(width), "{narrow:#?}");
            }
            assert!(
                narrow
                    .iter()
                    .any(|row| row == "   After Rename them: workflow = \"captain\""),
                "{narrow:#?}"
            );
        }

        // The pictured lines at 80 columns keep their columns, and the crew note wraps in its own.
        let pictured = question(
            ChoiceId::numbered(1),
            "~/.argonauts/config.toml",
            &renames_in(PICTURED),
        );
        let rows = rows_at(&pictured, 80);
        let right_at = 3 + r#"[approvals.goal] mode = "ask""#.len() + 6;
        let heading = carried(&rows, "   In the file now");
        assert_eq!(
            heading[0].find("After Rename them"),
            Some(right_at),
            "{rows:#?}"
        );
        let crew = carried(&rows, r#"   main_agent = "claude-code""#);
        assert!(crew.len() > 2, "the crew note wraps: {rows:#?}");
        for row in &crew[1..] {
            assert_eq!(row.len() - row.trim_start().len(), right_at, "{rows:#?}");
        }
    }

    #[test]
    fn a_rename_question_has_no_arrow_and_promises_no_values_it_does_not_show() {
        let choice = question(ChoiceId::numbered(1), "config.toml", &renames_in(OLDER));
        let body = choice.body.join("\n");
        assert!(!body.contains('→'), "{body}");
        assert!(!body.contains("before and after"), "{body}");
        assert!(body.contains("In the file now"), "{body}");
        assert!(
            body.contains("[approvals.goal] mode = \"supervised\""),
            "{body}"
        );
    }
}
