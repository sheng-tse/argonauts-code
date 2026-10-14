//! The setup's card: what would be written, and where it applies. Nothing is written until you
//! pick a place, and Not now or Esc writes nothing. A place your own words named in the same
//! message needs no card, and what was kept is said in the conversation instead.

use std::path::{Path, PathBuf};

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::checkpoint::{AnswerAction, AnswerOption};
use argonauts_core::layout::{project_files, user_files};
use argonauts_core::names::{ChoiceId, RoleName, TurnId, WorkerRef};
use argonauts_core::prompt_files::{self, PROJECT_DIR, PromptDirs, RoleFileMode};
use argonauts_core::prompt_parts::{CrewRole, Readers};
use argonauts_core::required::{self, RequiredStep};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::budget::OnHit;
use unicode_width::UnicodeWidthStr;

use super::form::{Place, Proposal, RoleChange};
use super::{Call, ChatState, fail};
use crate::choices::{self, choose, option};
use crate::controller::{Ctx, Effect};

/// A setup waiting on your pick, and the call waiting on the setup.
pub(super) struct Open {
    turn: TurnId,
    call: Call,
    proposal: Proposal,
}

const TITLE: &str = "Your setup";
const NOT_NOW: &str = "not_now";
const MODS_DIR: &str = "mods";

/// Shows the card. The call waits until you pick.
pub(super) fn open(ctx: &mut Ctx, chat: &mut ChatState, call: Call, proposal: Proposal) {
    let id = ctx.next_choice_id();
    let mut body: Vec<String> = rows(ctx, &proposal).iter().map(Row::marked).collect();
    body.push(String::new());
    body.extend(what_each_place_writes(ctx, &proposal));
    let mut choice = choices::app_choice(
        id.clone(),
        ChoiceKind::Setup,
        TITLE,
        body,
        options(ctx, &proposal),
    );
    choice.bell = true;
    chat.talk.cards.insert(
        id,
        Open {
            turn: call.turn.clone(),
            call,
            proposal,
        },
    );
    ctx.send(AppEvent::Choice { choice });
}

/// Keeps a setup where your own words said, with no card, and says what was kept.
pub(super) fn keep_as_said(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    call: Call,
    proposal: Proposal,
    place: Place,
) -> Vec<Effect> {
    let turn = call.turn.clone();
    let mut lines = vec![format!("{TITLE} · {}, as you said", label(place))];
    lines.extend(rows(ctx, &proposal).iter().map(Row::plain_text));
    ctx.notice(NoteLevel::Info, lines.join("\n"));
    match apply(ctx, chat, &turn, &proposal, place, Picked::NoCard) {
        Ok((effects, told)) => {
            call.answer(told);
            effects
        }
        Err(err) => {
            fail(
                call,
                format!("Writing it failed: {err:#}. Nothing else was changed."),
            );
            ctx.notice(NoteLevel::Warn, format!("The setup wasn't kept: {err:#}"));
            Vec::new()
        }
    }
}

pub(super) fn answer(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    id: ChoiceId,
    action: AnswerAction,
) -> anyhow::Result<Vec<Effect>> {
    let place = match &action {
        AnswerAction::Choose { value } if value == NOT_NOW => None,
        AnswerAction::Cancel => None,
        AnswerAction::Choose { value } => match Place::parse(value) {
            Some(place) => Some(place),
            None => anyhow::bail!("{value:?} isn't a place to keep a setup"),
        },
        other => anyhow::bail!("{other:?} doesn't say where to keep the setup"),
    };
    let Some(open) = chat.talk.cards.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    let Some(place) = place else {
        close_not_now(ctx, id, open);
        return Ok(Vec::new());
    };
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: label(place).to_owned(),
    });
    match apply(
        ctx,
        chat,
        &open.turn,
        &open.proposal,
        place,
        Picked::OnTheCard,
    ) {
        Ok((effects, told)) => {
            open.call.answer(told);
            Ok(effects)
        }
        Err(err) => {
            fail(
                open.call,
                format!(
                    "The user picked {}, and writing it failed: {err:#}",
                    label(place)
                ),
            );
            Err(err)
        }
    }
}

/// Esc, like Not now, writes nothing.
pub(super) fn not_now(ctx: &mut Ctx, chat: &mut ChatState, id: ChoiceId) -> Vec<Effect> {
    if let Some(open) = chat.talk.cards.remove(&id) {
        close_not_now(ctx, id, open);
    }
    Vec::new()
}

fn close_not_now(ctx: &Ctx, id: ChoiceId, open: Open) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: "Not now. Nothing changed.".to_owned(),
    });
    open.call
        .answer("The user said not now, so nothing changed. Don't ask again unless they do.");
}

fn label(place: Place) -> &'static str {
    match place {
        Place::Request => "This request",
        Place::Conversation => "This conversation",
        Place::Repository => "Save for this repository",
        Place::AllRepositories => "Save for all my repositories",
    }
}

fn options(ctx: &Ctx, proposal: &Proposal) -> Vec<AnswerOption> {
    let mut places = Vec::new();
    if proposal.request_too {
        places.push(('r', Place::Request));
    }
    places.extend([
        ('c', Place::Conversation),
        ('p', Place::Repository),
        ('e', Place::AllRepositories),
    ]);
    let mut options: Vec<AnswerOption> = places
        .into_iter()
        .enumerate()
        .map(|(at, (key, place))| {
            let mut picked = option(key, label(place), choose(place.value()), at == 0);
            picked.detail = detail(ctx, proposal, place);
            picked
        })
        .collect();
    options.push(option('n', "Not now", choose(NOT_NOW), false));
    options.push(option(
        'd',
        "Show what would be written",
        AnswerAction::ShowDiff,
        false,
    ));
    options
}

fn detail(ctx: &Ctx, proposal: &Proposal, place: Place) -> Option<String> {
    match place {
        Place::Request | Place::Conversation => {
            let waits = !proposal.prompts.is_empty() || !proposal.mods.is_empty();
            waits.then(|| "prompts and mods are kept only by a save".to_owned())
        }
        Place::Repository | Place::AllRepositories => {
            let files = files(ctx, proposal, place).len();
            let folder = match place {
                Place::Repository => format!("{PROJECT_DIR}/"),
                _ => ctx.env.display(ctx.env.paths.home()) + "/",
            };
            let writes = match files {
                1 => format!("writes 1 file in {folder}"),
                n => format!("writes {n} files in {folder}"),
            };
            Some(match place {
                Place::Repository => format!("{writes} · d shows the diff"),
                _ => writes,
            })
        }
    }
}

/// The card's table: what each part of the setup sets, the section first, as in
/// `limits  spend_per_day = 5.0`. The crew comes first and reads as the screen names agents,
/// `crew  orchestrator  Fable 5.1 on Claude Code`, then a role's instructions, then the rest.
/// A section is named once, and a note such as `follows the reviewers` stands in a column of
/// its own.
fn rows(ctx: &Ctx, proposal: &Proposal) -> Vec<Row> {
    let mut cells = crew_cells(ctx, proposal);
    for role in &proposal.roles {
        let verb = match role.mode {
            RoleFileMode::Add => "adds to",
            RoleFileMode::Replace => "replaces",
        };
        let mut cell = Cell::new(
            role.readers.as_str(),
            format!(
                "{}/{}.md   {}",
                prompt_files::ROLES_DIR,
                role.readers.as_str(),
                quoted(&role.text)
            ),
        );
        cell.verb = Some(verb);
        cells.push(cell);
    }
    for (key, value) in &proposal.settings {
        if Seat::of(key).is_some() || noted_on_its_seat(proposal, key) {
            continue;
        }
        let mut cell = match key.split_once('.') {
            Some((section, rest)) => Cell::new(section, format!("{rest} = {value}")),
            None => Cell::new(key, value.clone()),
        };
        cell.note = limit_note(ctx, proposal, key);
        cells.push(cell);
    }
    for prompt in &proposal.prompts {
        cells.push(Cell::new(
            "prompt",
            format!(
                "{}/{}.md   {}",
                prompt_files::PROMPTS_DIR,
                prompt.template.id,
                quoted(&prompt.text)
            ),
        ));
    }
    for file in &proposal.mods {
        cells.push(Cell::new("mod", format!("{MODS_DIR}/{}.toml", file.name)));
    }
    for step in &proposal.steps {
        let mut cell = Cell::new(
            "workflow",
            format!(
                "{}/{}   {}",
                project_files::WORKFLOWS,
                required::FILE,
                step_said(step)
            ),
        );
        cell.verb = Some("adds to");
        cells.push(cell);
    }
    let width = cells
        .iter()
        .map(|cell| cell.section.width())
        .max()
        .unwrap_or(0)
        .max(12)
        + 2;
    // Notes line up past the longest text that has one.
    let noted = cells
        .iter()
        .filter(|cell| cell.note.is_some())
        .map(Cell::width)
        .max()
        .unwrap_or(0)
        + 3;
    let mut last = String::new();
    cells
        .into_iter()
        .map(|cell| {
            let shown = if cell.section == last {
                ""
            } else {
                &cell.section
            };
            let mut row = Row::default();
            row.plain(format!("{shown}{}", " ".repeat(width - shown.width())));
            if let Some(verb) = cell.verb {
                row.dim(verb.to_owned());
                row.plain(" ".to_owned());
            }
            row.plain(cell.text.clone());
            if let Some(note) = &cell.note {
                row.plain(" ".repeat(noted - cell.width()));
                row.dim(note.clone());
            }
            last = cell.section;
            row
        })
        .collect()
}

/// One row of the card's table.
struct Cell {
    section: String,
    /// Said before the text and dim, such as `adds to`.
    verb: Option<&'static str>,
    text: String,
    /// Said after it, such as `follows the reviewers`.
    note: Option<String>,
}

impl Cell {
    fn new(section: &str, text: String) -> Self {
        Self {
            section: section.to_owned(),
            verb: None,
            text,
            note: None,
        }
    }

    fn width(&self) -> usize {
        self.verb.map_or(0, |verb| verb.width() + 1) + self.text.width()
    }
}

/// A row as the card draws it: the settings in plain text and only the notes dim.
#[derive(Debug, Default)]
struct Row {
    /// Each part, and whether it's dim.
    parts: Vec<(String, bool)>,
}

impl Row {
    fn plain(&mut self, text: String) {
        self.parts.push((text, false));
    }

    fn dim(&mut self, text: String) {
        self.parts.push((text, true));
    }

    fn plain_text(&self) -> String {
        self.parts.iter().map(|(text, _)| text.as_str()).collect()
    }

    /// Split by tabs into parts, plain then dim in turn, which is how the card tells a note.
    fn marked(&self) -> String {
        let mut out = String::new();
        let mut dim = false;
        for (text, is_dim) in &self.parts {
            if *is_dim != dim {
                out.push('\t');
                dim = *is_dim;
            }
            out.push_str(text);
        }
        out
    }
}

/// A role's `goal` the card says on that role's crew row instead, as
/// `workers  GPT 5.6 on Codex  each task as Codex's own goal`.
fn noted_on_its_seat(proposal: &Proposal, key: &str) -> bool {
    goal_role(key).is_some_and(|role| {
        proposal
            .settings
            .iter()
            .any(|(other, _)| Seat::of(other).is_some_and(|seat| seat.role() == Some(role)))
    })
}

/// The role a `roles.<role>.goal` key sets.
fn goal_role(key: &str) -> Option<RoleName> {
    RoleName::parse(key.strip_prefix("roles.")?.strip_suffix(".goal")?)
}

/// How a role works each task, by its `goal` value and the CLI its first agent runs on.
fn goal_note(value: &str, agents: Option<&WorkerRef>, ctx: &Ctx) -> Option<String> {
    match value.trim_matches('"') {
        "cli" => Some(match agents {
            Some(agent) => format!(
                "each task as {}'s own goal",
                ctx.names.harness(&agent.worker)
            ),
            None => "each task as its CLI's own goal".to_owned(),
        }),
        "argonauts" => Some("each task as a goal Argonauts keeps".to_owned()),
        "off" => Some("no goal per task".to_owned()),
        _ => None,
    }
}

/// A limit's note: what happens when it's reached, unless the setup says that itself.
fn limit_note(ctx: &Ctx, proposal: &Proposal, key: &str) -> Option<String> {
    const LIMITS: [&str; 3] = [
        "limits.spend_per_run",
        "limits.spend_per_day",
        "limits.plan_usage",
    ];
    if !LIMITS.contains(&key)
        || proposal
            .settings
            .iter()
            .any(|(set, _)| set == "limits.when_reached")
    {
        return None;
    }
    let when = match ctx.settings().limits.budget.when_reached {
        OnHit::Fallback => "fallback",
        OnHit::Stop => "stop",
    };
    Some(format!("when reached: {when}"))
}

const CREW: &str = "crew";

/// A crew setting: `main_agent`, the `crew` itself, or who fills a role
/// (`roles.<role>.workers`). They sort in this order, a role by the crew role whose work it
/// does, and that crew role's own seat before its finer roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Seat {
    Main,
    Members,
    Role {
        crew: CrewRole,
        finer: bool,
        role: RoleName,
    },
}

impl Seat {
    fn of(key: &str) -> Option<Self> {
        match key {
            "main_agent" => Some(Self::Main),
            CREW => Some(Self::Members),
            _ if key.starts_with("crew.") => {
                let role = match &key["crew.".len()..] {
                    "orchestrator" => RoleName::Orchestrator,
                    "workers" => RoleName::Worker,
                    "reviewers" => RoleName::Reviewer,
                    "judge" => RoleName::Judge,
                    _ => return None,
                };
                Some(Self::crew(role))
            }
            _ => {
                let role = key.strip_prefix("roles.")?.strip_suffix(".workers")?;
                let role = RoleName::parse(role)?;
                let crew = CrewRole::read_by(role);
                let finer = Self::crew_word(role).is_none();
                Some(Self::Role { crew, finer, role })
            }
        }
    }

    /// The seat of one of the five roles itself, as `crew.<role>` or `roles.<role>.workers` set it.
    fn crew(role: RoleName) -> Self {
        Self::Role {
            crew: CrewRole::read_by(role),
            finer: false,
            role,
        }
    }

    fn role(self) -> Option<RoleName> {
        match self {
            Self::Role { role, .. } => Some(role),
            Self::Main | Self::Members => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Members => "members",
            Self::Role { role, .. } => Self::crew_word(role).unwrap_or(role.as_str()),
        }
    }

    /// The word the screen uses for one of the crew's five roles, `workers` for the worker
    /// role. A finer role has none and keeps its own name.
    fn crew_word(role: RoleName) -> Option<&'static str> {
        let crew = match role {
            RoleName::Main => CrewRole::Main,
            RoleName::Orchestrator => CrewRole::Orchestrator,
            RoleName::Worker => CrewRole::Workers,
            RoleName::Reviewer => CrewRole::Reviewers,
            RoleName::Judge => CrewRole::Judge,
            _ => return None,
        };
        Some(crew.as_str())
    }
}

/// The crew settings among the setup's, as `crew  orchestrator   Fable 5.1 on Claude Code`. A
/// role's `goal` is said on its row. A role that follows another and has no seat of its own
/// shows with the seats it follows, as the judge does the reviewers'.
fn crew_cells(ctx: &Ctx, proposal: &Proposal) -> Vec<Cell> {
    let mut seats: Vec<(Seat, String, Option<String>)> = proposal
        .settings
        .iter()
        .filter_map(|(key, value)| {
            let seat = Seat::of(key)?;
            let goal = seat.role().and_then(|role| {
                let (_, goal) = proposal
                    .settings
                    .iter()
                    .find(|(key, _)| goal_role(key) == Some(role))?;
                goal_note(goal, first_agent(value).as_ref(), ctx)
            });
            Some((seat, agents_named(ctx, value), goal))
        })
        .collect();
    let crew = &ctx.settings().crew;
    for (follower, leader) in [
        (CrewRole::Workers, CrewRole::Orchestrator),
        (CrewRole::Judge, CrewRole::Reviewers),
    ] {
        let seat_of = |role: CrewRole| {
            seats
                .iter()
                .find(|(seat, ..)| matches!(seat, Seat::Role { crew: seat_crew, finer: false, .. } if *seat_crew == role))
        };
        let follows = crew.followed(follower) == Some(leader);
        if let (true, None, Some((_, agents, _))) = (follows, seat_of(follower), seat_of(leader)) {
            let role = match follower {
                CrewRole::Workers => RoleName::Worker,
                _ => RoleName::Judge,
            };
            let note = format!("follows the {}", leader.as_str());
            seats.push((Seat::crew(role), agents.clone(), Some(note)));
        }
    }
    seats.sort_by_key(|(seat, ..)| *seat);
    let width = seats
        .iter()
        .map(|(seat, ..)| seat.word().len())
        .max()
        .unwrap_or(0)
        + 3;
    seats
        .into_iter()
        .map(|(seat, agents, note)| Cell {
            section: CREW.to_owned(),
            verb: None,
            text: format!("{:width$}{agents}", seat.word()),
            note,
        })
        .collect()
}

/// The first agent a crew setting's TOML value names.
fn first_agent(value: &str) -> Option<WorkerRef> {
    let parsed = toml::from_str::<toml::Table>(&format!("v = {value}"))
        .ok()?
        .remove("v")?;
    let id = match parsed {
        toml::Value::String(id) => id,
        toml::Value::Array(ids) => ids.first()?.as_str()?.to_owned(),
        _ => return None,
    };
    id.parse().ok()
}

/// A crew setting's TOML value, one agent or a list, named as the screen names agents. A text
/// that names no agent is shown as it is.
fn agents_named(ctx: &Ctx, value: &str) -> String {
    let named = |id: &str| match id.parse::<WorkerRef>() {
        Ok(worker) => ctx.names.worker(&worker),
        Err(_) => id.to_owned(),
    };
    let parsed = toml::from_str::<toml::Table>(&format!("v = {value}"))
        .ok()
        .and_then(|mut table| table.remove("v"));
    match parsed {
        Some(toml::Value::String(id)) => named(&id),
        Some(toml::Value::Array(ids)) if ids.is_empty() => "picked from the crew".to_owned(),
        Some(toml::Value::Array(ids)) => {
            let names: Option<Vec<String>> = ids.iter().map(|id| id.as_str().map(&named)).collect();
            names.map_or_else(|| value.to_owned(), |names| names.join(", "))
        }
        _ => value.to_owned(),
    }
}

/// `"Benchmark" before sign-off, by the workers`.
fn step_said(step: &RequiredStep) -> String {
    let when = step.when.as_deref().unwrap_or("before the sign-off");
    format!("\"{}\" {when}, by {}", step.name, step.role.the_agents())
}

/// The first line of a text, cut short for a row.
fn quoted(text: &str) -> String {
    const MAX: usize = 60;
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim();
    let more = text.trim().lines().count() > 1;
    let mut shown: String = line.chars().take(MAX).collect();
    if more || line.chars().count() > MAX {
        shown.push('…');
    }
    format!("\"{shown}\"")
}

/// For Show what would be written: what you asked, then what each place would write.
fn what_each_place_writes(ctx: &Ctx, proposal: &Proposal) -> Vec<String> {
    let mut lines = vec![format!("You asked: {}", proposal.asked), String::new()];
    for place in Place::ALL {
        if place == Place::Request && !proposal.request_too {
            continue;
        }
        lines.push(label(place).to_owned());
        match place {
            Place::Request | Place::Conversation => {
                for (key, value) in session_settings(ctx, proposal) {
                    lines.push(format!("  {key} = {value}"));
                }
                for step in &proposal.steps {
                    lines.push(format!("  every workflow has {}", step_said(step)));
                }
                if !proposal.prompts.is_empty() || !proposal.mods.is_empty() {
                    lines.push("  Prompts and mods are kept only by a save.".to_owned());
                }
            }
            Place::Repository | Place::AllRepositories => {
                for (file, said) in files(ctx, proposal, place) {
                    lines.push(format!("  {}", ctx.env.shown(&file)));
                    lines.extend(said.into_iter().map(|line| format!("    {line}")));
                }
            }
        }
        lines.push(String::new());
    }
    lines.pop();
    lines
}

/// Each file a save to `place` writes, with what it gets.
fn files(ctx: &Ctx, proposal: &Proposal, place: Place) -> Vec<(PathBuf, Vec<String>)> {
    let Some(dir) = folder(ctx, place) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    if !proposal.settings.is_empty() {
        let config = match place {
            Place::Repository => ctx.env.project_config(),
            _ => ctx.env.global_config(),
        };
        let said = proposal
            .settings
            .iter()
            .map(|(key, value)| format!("{key} = {value}"))
            .collect();
        files.push((config, said));
    }
    for role in &proposal.roles {
        let said = match role.mode {
            RoleFileMode::Add => format!("adds {}", quoted(&role.text)),
            RoleFileMode::Replace => format!("replaces what it reads with {}", quoted(&role.text)),
        };
        files.push((role_path(&dir, role.readers), vec![said]));
    }
    for prompt in &proposal.prompts {
        let path = dir
            .join(prompt_files::PROMPTS_DIR)
            .join(format!("{}.md", prompt.template.id));
        files.push((path, vec![quoted(&prompt.text)]));
    }
    for file in &proposal.mods {
        let path = dir.join(MODS_DIR).join(format!("{}.toml", file.name));
        files.push((path, Vec::new()));
    }
    if let Some(path) = always_file(&dir, place).filter(|_| !proposal.steps.is_empty()) {
        let said = proposal
            .steps
            .iter()
            .map(|step| format!("adds {}", step_said(step)))
            .collect();
        files.push((path, said));
    }
    files
}

/// `workflows/always.toml` in the folder a save to `place` writes in.
fn always_file(dir: &Path, place: Place) -> Option<PathBuf> {
    let workflows = match place {
        Place::Repository => project_files::WORKFLOWS,
        Place::AllRepositories => user_files::WORKFLOWS,
        Place::Request | Place::Conversation => return None,
    };
    Some(dir.join(workflows).join(required::FILE))
}

/// `.argonauts/` for the repository, `~/.argonauts/` for all of them.
fn folder(ctx: &Ctx, place: Place) -> Option<PathBuf> {
    match place {
        Place::Repository => Some(ctx.env.repo_root.join(PROJECT_DIR)),
        Place::AllRepositories => Some(ctx.env.paths.home().to_path_buf()),
        Place::Request | Place::Conversation => None,
    }
}

fn role_path(dir: &Path, readers: Readers) -> PathBuf {
    dir.join(prompt_files::ROLES_DIR)
        .join(format!("{}.md", readers.as_str()))
}

/// Writes the setup to `place`. Settings go through the steps `/config` takes, so a safety key
/// still asks you to confirm. Returns those steps and what the session is told.
fn apply(
    ctx: &mut Ctx,
    chat: &mut ChatState,
    turn: &TurnId,
    proposal: &Proposal,
    place: Place,
    picked: Picked,
) -> anyhow::Result<(Vec<Effect>, String)> {
    match place {
        Place::Request => {
            chat.talk.for_request = Some((turn.clone(), session_settings(ctx, proposal)));
            chat.talk.steps_for_request = Some((turn.clone(), proposal.steps.clone()));
            Ok((
                Vec::new(),
                format!(
                    "The user kept it for this request: the workflow this message starts uses \
                     it, and nothing after it does.{} Start that workflow now.",
                    left_out(proposal)
                ),
            ))
        }
        Place::Conversation => {
            for step in &proposal.steps {
                chat.talk
                    .steps_for_conversation
                    .retain(|known| !required::same_name(&known.name, &step.name));
                chat.talk.steps_for_conversation.push(step.clone());
            }
            let pairs = session_settings(ctx, proposal);
            let effects = save(pairs, SettingScope::Session, picked);
            Ok((
                effects,
                format!(
                    "The user kept it for this conversation, until they quit Argonauts.{}",
                    left_out(proposal)
                ),
            ))
        }
        Place::Repository | Place::AllRepositories => {
            let written = write_files(ctx, proposal, place)?;
            // The turn's end puts back what the session wrote in `.argonauts/`, never this.
            let mut kept = written.clone();
            if !proposal.settings.is_empty() && place == Place::Repository {
                kept.push(ctx.env.project_config());
            }
            crate::controller::chat::direct::setup_wrote(chat, turn, &kept);
            let scope = match place {
                Place::Repository => SettingScope::Project,
                _ => SettingScope::Global,
            };
            let effects = save(proposal.settings.clone(), scope, picked);
            let mut shown: Vec<String> = written.iter().map(|path| ctx.env.shown(path)).collect();
            if !proposal.settings.is_empty() {
                let config = match place {
                    Place::Repository => ctx.env.project_config(),
                    _ => ctx.env.global_config(),
                };
                shown.insert(0, ctx.env.shown(&config));
            }
            if !written.is_empty() {
                let names: Vec<String> = written.iter().map(|path| ctx.env.shown(path)).collect();
                ctx.notice(NoteLevel::Info, format!("Wrote {}", names.join(", ")));
            }
            let whose = match place {
                Place::Repository => "this repository",
                _ => "all their repositories",
            };
            Ok((
                effects,
                format!("The user saved it for {whose}: {}.", shown.join(", ")),
            ))
        }
    }
}

/// How the place was chosen. A pick on the card is the confirmation, so a limit or other
/// safety key asks nothing more; a place only your words named still asks for a safety key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picked {
    OnTheCard,
    NoCard,
}

fn save(pairs: Vec<(String, String)>, scope: SettingScope, picked: Picked) -> Vec<Effect> {
    if pairs.is_empty() {
        return Vec::new();
    }
    match picked {
        Picked::OnTheCard => vec![Effect::SaveConfirmedSettings { pairs, scope }],
        Picked::NoCard => vec![Effect::SaveSettings { pairs, scope }],
    }
}

fn left_out(proposal: &Proposal) -> &'static str {
    if proposal.prompts.is_empty() && proposal.mods.is_empty() {
        ""
    } else {
        " Prompts and mods are kept only by a save to a repository or to all of them, so \
         they weren't written."
    }
}

/// The setup as settings alone, for a place with no files: a role's text goes to
/// `roles.<role>.instructions` of each role that reads it, after what that key held.
fn session_settings(ctx: &Ctx, proposal: &Proposal) -> Vec<(String, String)> {
    let mut pairs = proposal.settings.clone();
    for change in &proposal.roles {
        for role in readers_of(change) {
            let before = ctx
                .settings()
                .roles
                .get(&role)
                .and_then(|settings| settings.instructions.clone())
                .filter(|text| !text.trim().is_empty());
            let text = match (change.mode, before) {
                (RoleFileMode::Add, Some(before)) => format!("{before}\n{}", change.text),
                _ => change.text.clone(),
            };
            let value = toml::Value::String(text).to_string();
            pairs.push((format!("roles.{}.instructions", role.as_str()), value));
        }
    }
    pairs
}

/// The roles that read a role's file: every finer role of a crew role, or the one finer role.
fn readers_of(change: &RoleChange) -> Vec<RoleName> {
    match change.readers {
        Readers::Finer(role) => vec![role],
        Readers::Crew(crew) => RoleName::ALL
            .into_iter()
            .filter(|role| CrewRole::read_by(*role) == crew)
            .collect(),
    }
}

/// Writes the role, prompt and mod files. The project's role and prompt files are yours, so
/// they're trusted as they're written.
fn write_files(ctx: &Ctx, proposal: &Proposal, place: Place) -> anyhow::Result<Vec<PathBuf>> {
    let Some(dir) = folder(ctx, place) else {
        return Ok(Vec::new());
    };
    let dirs = match place {
        Place::Repository => PromptDirs::new(Some(&ctx.env.repo_root), None),
        _ => PromptDirs::new(None, Some(ctx.env.paths.home())),
    };
    let mut written = Vec::new();
    for change in &proposal.roles {
        let (mode, text) = match dirs.own_role(change.readers) {
            Some(Ok(file)) if change.mode == RoleFileMode::Add && !file.text.trim().is_empty() => (
                file.mode,
                format!("{}\n\n{}", file.text.trim(), change.text),
            ),
            _ => (change.mode, change.text.clone()),
        };
        written.push(prompt_files::save_role_file(
            &dir,
            change.readers,
            mode,
            &text,
        )?);
    }
    for prompt in &proposal.prompts {
        written.push(prompt_files::save_template(
            &dir,
            &prompt.template,
            &prompt.text,
        )?);
    }
    if place == Place::Repository && !written.is_empty() {
        argonauts_crew::prompts::trust::trust(&ctx.env.state_dir, &written)?;
    }
    // A mod is written as `/mods` saves one: whole, and only when it loads.
    for file in &proposal.mods {
        let path =
            argonauts_core::settings::mods::save::save(&dir.join(MODS_DIR), &file.name, &file.text)
                .map_err(|problems| anyhow::anyhow!("{}: {}", file.name, problems.join("; ")))?;
        written.push(path);
    }
    if let Some(path) = always_file(&dir, place).filter(|_| !proposal.steps.is_empty()) {
        argonauts_registry::save_required_steps(&path, &proposal.steps)
            .map_err(anyhow::Error::msg)?;
        written.push(path);
    }
    Ok(written)
}
