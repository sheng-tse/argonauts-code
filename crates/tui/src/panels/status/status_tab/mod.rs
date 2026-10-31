//! The Status tab: what runs here and how it is set up, for every CLI at once. The facts come
//! first as label and value, then a card for the CLIs, one for the settings in effect, and one
//! for the runs in this folder.

use argonauts_core::app::status_tab::{
    CliStatus, StatusTabView, VersionNoteKind, VersionStanding, asking_for_providers,
    providers_unread,
};
use argonauts_core::base::text::shown_name;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::look::logos::logo_for;
use argonauts_core::runs::api::{RunState, RunSummary};
use argonauts_core::settings::layer;
use ratatui::text::Span;
use time::OffsetDateTime;

use crate::base::format::{self, WallClock};
use crate::base::kit::logo::{ModelLabel, NoPicture};
use crate::base::kit::{Drawn, Frame, Header, Kit, spread};
use crate::base::text::{sanitize_line, spans_width, str_width, truncate_spans};
use crate::base::theme::Role;

/// The runs card when this folder has none.
pub const NO_RUNS: &str =
    "None yet. A workflow you start shows here, with what it's doing and anything waiting for you.";

/// The labels' column of the facts at the top.
const FACT_LABELS: usize = 16;
/// The columns of the CLIs card: the dot and the name, then the version.
const CLI_NAMES: usize = 18;
const CLI_VERSIONS: usize = 10;
/// The settings card's first column.
const SETTING_NAMES: usize = 32;
/// The runs card lists this many, the newest first.
const RUNS_LISTED: usize = 5;

/// What the tab draws from, and how it reads times.
pub struct StatusTabCards<'a> {
    pub view: &'a StatusTabView,
    pub names: &'a DisplayNames,
    pub clock: WallClock,
    /// The approval mode the bottom line shows, which may be this session's own.
    pub mode: Option<&'a str>,
}

impl StatusTabCards<'_> {
    /// Version, conversation, folder, main session, workflow and mode, a row each. A fact too
    /// long for the tab runs on to a row under its words.
    pub fn facts(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let view = self.view;
        let text = |words: String| vec![kit.span(words, Role::Text)];
        let dim = |words: String| vec![kit.dim(words)];
        let in_cli = view
            .in_cli
            .as_ref()
            .map(|cli| (format!("In {}", sanitize_line(&cli.cli)), cli));
        let mut facts: Vec<(&str, Vec<Vec<Span<'static>>>)> = vec![(
            "Version",
            vec![text(format!("Argonauts Code {}", view.version))],
        )];
        if let Some(conversation) = &view.conversation {
            // A time of day as the app writes one: `7:12 PM`.
            let started =
                crate::base::format::ClockTime::from(self.clock.to_local(conversation.started));
            facts.push((
                "Conversation",
                vec![
                    text(conversation.id.clone()),
                    dim(format!("started {started}")),
                ],
            ));
        }
        let git = if view.folder.git {
            "git"
        } else {
            "no git, workflows keep a hidden history"
        };
        let mut folder = vec![text(view.folder.shown.clone()), dim(git.to_owned())];
        // What the hidden history takes, and the command that trims it.
        if let Some(size) = view
            .folder
            .history_size
            .as_ref()
            .filter(|_| !view.folder.git)
        {
            folder.push(vec![
                kit.dim(format!("history.git {size}{BETWEEN}")),
                kit.span("/git clean", Role::Text),
            ]);
        }
        facts.push(("Folder", folder));
        if let Some(main) = &view.main {
            let worker = &main.worker;
            let label = ModelLabel::of(self.names, worker, kit.theme, NoPicture::Nothing);
            let mut named = label.logo;
            let name = if label.pictured {
                label.name
            } else {
                self.names.worker(worker)
            };
            named.extend(text(name));
            let mut session = vec![named];
            if let Some(effort) = &main.effort {
                session.push(dim(format!("{effort} effort")));
            }
            match (main.context_used, main.context_window) {
                (Some(used), Some(window)) => session.push(dim(format!(
                    "{} of {} context",
                    format::tokens_beside(used, window),
                    format::window(window)
                ))),
                (Some(used), None) => {
                    session.push(dim(format!("{} in context", format::tokens(used))))
                }
                _ => {}
            }
            facts.push(("Main session", session));
        }
        // What to type in the session's own CLI to carry it on there, and the key that opens it.
        if let Some((label, cli)) = &in_cli {
            facts.push((
                label.as_str(),
                vec![
                    text(sanitize_line(&cli.command).into_owned()),
                    dim("o opens it".to_owned()),
                ],
            ));
        }
        let ultracode = if view.workflow.ultracode { "on" } else { "off" };
        facts.push((
            "Workflow",
            vec![
                text(shown_name(&view.workflow.name)),
                dim(format!("ultracode {ultracode}")),
                dim(format!("workflow size {}", view.workflow.size)),
            ],
        ));
        let mode = self.mode.unwrap_or(&view.mode);
        facts.push(("Mode", vec![text(shown_name(mode))]));
        let mut drawn = Drawn::new();
        for (label, parts) in facts {
            let label = vec![Span::raw(" "), kit.dim(padded(label, FACT_LABELS))];
            let mut rows = PartRows::after(kit, label, 1 + FACT_LABELS, width);
            for part in parts {
                rows.push(part);
            }
            for row in rows.rows {
                drawn.push(truncate_spans(&row, width));
            }
        }
        drawn
    }

    /// `◆ your CLIs`: each CLI with its version, who it's signed in as or what it can use, and
    /// how that version stands. The main session's CLI has the filled dot.
    pub fn clis(&self, kit: &Kit<'_>, frame: Frame) -> Drawn {
        let inner = frame.inner();
        let mut body = Drawn::new();
        body.push(heading(
            kit,
            "your CLIs",
            "who you're signed in as, or what each can use",
            inner,
        ));
        if self.view.clis.is_empty() {
            body.push(vec![
                kit.dim("Doctor hasn't checked the CLIs yet; /doctor checks them."),
            ]);
        }
        for cli in &self.view.clis {
            for row in cli_rows(kit, cli, inner) {
                body.push(truncate_spans(&row, inner));
            }
        }
        frame.draw(kit, body)
    }

    /// `◆ settings in effect`: each file and how many keys it sets, the mods, the main
    /// session's tools and its MCP servers. Enter opens All settings with every key.
    pub fn settings(&self, kit: &Kit<'_>, frame: Frame) -> Drawn {
        let inner = frame.inner();
        let settings = &self.view.settings;
        let mut body = Drawn::new();
        let right = "enter opens All settings: each key and where it came from";
        body.push(heading(kit, "settings in effect", right, inner));
        let mut row = |name: String, value: String| {
            let row = vec![
                kit.span(padded(&name, SETTING_NAMES), Role::Text),
                kit.dim(value),
            ];
            body.push(truncate_spans(&row, inner));
        };
        if settings.files.is_empty() {
            row(
                "settings files".to_owned(),
                "none · every key is built in".to_owned(),
            );
        }
        for file in &settings.files {
            let keys = match file.keys {
                1 => "1 key".to_owned(),
                n => format!("{n} keys"),
            };
            row(
                file.path.clone(),
                format!("{keys} · {}", layer_words(&file.layer)),
            );
        }
        let yours = match settings.mods_yours.as_slice() {
            [] => String::new(),
            names => format!(" · {} of yours ({})", names.len(), names.join(", ")),
        };
        row(
            "mods".to_owned(),
            format!("{} built in{yours}", settings.mods_built_in),
        );
        let tools = if settings.tools_off {
            "off · main_session.tools = false".to_owned()
        } else if settings.tools.is_empty() {
            "none, since its CLI takes no tools from Argonauts Code".to_owned()
        } else {
            settings.tools.join(" · ")
        };
        row("tools for the main session".to_owned(), tools);
        let cli = self
            .view
            .main
            .as_ref()
            .map(|main| self.names.cli(&main.worker.agent))
            .unwrap_or_else(|| "its CLI".to_owned());
        let servers = match &settings.mcp_servers {
            None => "listed once the main session starts".to_owned(),
            Some(servers) if servers.is_empty() => format!("none in your {cli} settings"),
            Some(servers) => format!(
                "{} · from your {cli} settings: {}",
                servers.len(),
                servers.join(", ")
            ),
        };
        row("MCP servers".to_owned(), servers);
        frame.draw(kit, body)
    }

    /// `◆ runs in this folder`: the newest few with what each is doing, or one quiet line.
    pub fn runs(&self, kit: &Kit<'_>, frame: Frame) -> Drawn {
        let inner = frame.inner();
        let mut body = Drawn::new();
        body.push(heading(kit, "runs", "in this folder", inner));
        if self.view.runs.is_empty() {
            body.push(vec![kit.dim(NO_RUNS)]);
        }
        for run in self.view.runs.iter().take(RUNS_LISTED) {
            let day = day(self.clock.to_local(run.updated));
            body.push(truncate_spans(&run_row(kit, run, &day), inner));
        }
        if let Some(more) = self
            .view
            .runs
            .len()
            .checked_sub(RUNS_LISTED)
            .filter(|n| *n > 0)
        {
            body.push(vec![
                kit.dim(format!("{more} more · /resume lists them all")),
            ]);
        }
        frame.draw(kit, body)
    }
}

/// `◆ title` at the left of a card's first row, and what it's about, dim, at the right.
fn heading(kit: &Kit<'_>, title: &str, about: &str, width: usize) -> Vec<Span<'static>> {
    let left = Header::new(title, Role::Question).left(kit);
    let right = vec![kit.dim(about.to_owned())];
    if spans_width(&left) + spans_width(&right) + 2 > width {
        return truncate_spans(&left, width);
    }
    spread(left, right, width)
}

/// What stands between two parts of a row.
const BETWEEN: &str = " · ";

/// Rows of parts with a dim ` · ` between them. A part that doesn't fit what is left of a row
/// starts the next one, set in under the first row's words, so a narrow tab drops no part and
/// cuts only one too long for a row of its own.
struct PartRows<'a> {
    kit: &'a Kit<'a>,
    /// The cells a row may fill.
    width: usize,
    /// The blank cells that start every row after the first.
    set_in: usize,
    rows: Vec<Vec<Span<'static>>>,
    /// The row being filled has no part yet.
    fresh: bool,
}

impl<'a> PartRows<'a> {
    /// The first row starts with `opening`, which is `set_in` cells wide.
    fn after(kit: &'a Kit<'a>, opening: Vec<Span<'static>>, set_in: usize, width: usize) -> Self {
        Self {
            kit,
            width,
            set_in,
            rows: vec![opening],
            fresh: true,
        }
    }

    /// Whether all of `parts` go on the row being filled.
    fn has_room_for(&self, parts: &[Vec<Span<'static>>]) -> bool {
        let between = str_width(BETWEEN);
        let mut needed: usize = parts.iter().map(|part| between + spans_width(part)).sum();
        if self.fresh {
            needed = needed.saturating_sub(between);
        }
        let used = self.rows.last().map_or(0, |row| spans_width(row));
        used + needed <= self.width
    }

    fn next_row(&mut self) {
        if !self.fresh {
            self.rows.push(vec![Span::raw(" ".repeat(self.set_in))]);
            self.fresh = true;
        }
    }

    fn push(&mut self, part: Vec<Span<'static>>) {
        if !self.has_room_for(std::slice::from_ref(&part)) {
            self.next_row();
        }
        let between = (!self.fresh).then(|| self.kit.dim(BETWEEN));
        if let Some(row) = self.rows.last_mut() {
            row.extend(between);
            row.extend(part);
            self.fresh = false;
        }
    }
}

/// `● Claude Code     2.1.296   you@example.com · Max · checked · works`: the CLI's logo, or
/// where no picture draws the dot in its colour, filled for the main session's CLI, its name, its version, who it's signed in as or what it can use, dim, then how its
/// version stands in that standing's colour. A second row, set in under the first's words,
/// holds a provider CLI's default model and what a new major version was checked with. In a
/// card `inner` cells across that the first row is too long for, how the version stands has
/// the row under who is signed in.
fn cli_rows(kit: &Kit<'_>, cli: &CliStatus, inner: usize) -> Vec<Vec<Span<'static>>> {
    let dot = if cli.main { "● " } else { "○ " };
    let label = |mark: Span<'static>| {
        ModelLabel::cli_named(&cli.id, &cli.name, kit.theme, NoPicture::Mark(mark))
    };
    let dim = label(kit.dim(dot));
    // The name's column ends where it would after a dot, whatever stands before it.
    let name = padded(&dim.name, CLI_NAMES.saturating_sub(spans_width(&dim.logo)));
    if !cli.installed {
        let mut row = dim.logo;
        row.extend([
            kit.span(name, Role::Text),
            Span::raw(padded("—", CLI_VERSIONS)),
            kit.dim("not installed"),
            kit.dim(" · /update can't add it"),
        ]);
        return vec![row];
    }
    let mut opening = label(kit.span(dot, Role::for_cli(&cli.id))).logo;
    opening.extend([
        kit.span(name, Role::Text),
        kit.dim(padded(cli.version.as_deref().unwrap_or("?"), CLI_VERSIONS)),
    ]);
    let mut rows = PartRows::after(kit, opening, CLI_NAMES + CLI_VERSIONS, inner);
    for part in who(kit, cli) {
        rows.push(part);
    }

    let mut standing = vec![vec![
        kit.span(cli.check.state(), standing_role(cli.check.standing)),
    ]];
    let note = cli.check.note();
    // Up to date and a match are what the state already says; an update is worth the row.
    if let Some(note) = note
        .as_ref()
        .filter(|note| matches!(note.kind, VersionNoteKind::Needed | VersionNoteKind::Out))
    {
        standing.push(vec![kit.dim(note.words.clone())]);
        standing.extend(
            note.command
                .as_ref()
                .map(|command| vec![kit.span(command.clone(), Role::Text)]),
        );
    }
    // The end of the row is the command that updates the CLI, which a cut would take first.
    if !rows.has_room_for(&standing) {
        rows.next_row();
    }
    for part in standing {
        rows.push(part);
    }

    let default_model = cli.default_model.as_ref();
    let mut under: Vec<String> = Vec::new();
    under.extend(default_model.map(|model| format!("default {model}")));
    under.extend(
        note.filter(|note| note.kind == VersionNoteKind::Compared)
            .map(|note| note.words),
    );
    if !under.is_empty() {
        rows.next_row();
    }
    for words in under {
        rows.push(vec![kit.dim(words)]);
    }
    rows.rows
}

/// Who a CLI is signed in as, with its plan, or the providers it can use and how each bills,
/// as the parts of its row.
fn who(kit: &Kit<'_>, cli: &CliStatus) -> Vec<Vec<Span<'static>>> {
    if let Some(providers) = &cli.providers {
        if providers.is_empty() {
            return vec![vec![kit.span(no_provider(&cli.command), Role::Text)]];
        }
        let named = providers.iter().map(|provider| {
            let logo = provider_logo(&provider.name);
            let label = ModelLabel::whole(&logo, &provider.name, kit.theme, NoPicture::Nothing);
            let mut named = label.logo;
            named.extend([
                kit.span(label.name, Role::Text),
                kit.dim(format!(" ({})", provider.billing.words())),
            ]);
            named
        });
        return named.collect();
    }
    // A CLI with providers of its own has no one account, so nothing is said of an address.
    if cli.lists_providers {
        return vec![if cli.providers_unread {
            vec![kit.span(providers_unread(&cli.command), Role::Warn)]
        } else {
            vec![kit.dim(asking_for_providers(&cli.name))]
        }];
    }
    let with_plan = |account: String| {
        let words = std::iter::once(account).chain(cli.plan.clone());
        words.map(|words| vec![kit.dim(words)]).collect()
    };
    match &cli.account {
        Some(account) => with_plan(account.clone()),
        None if cli.untrusted_folder => {
            vec![vec![kit.span(untrusted_folder(&cli.command), Role::Warn)]]
        }
        None if cli.signed_out => {
            vec![vec![
                kit.span("not signed in · /login signs in", Role::Warn),
            ]]
        }
        None => with_plan(argonauts_core::app::status_tab::UNNAMED_ACCOUNT.to_owned()),
    }
}

/// The logo of a provider a CLI can use, known by its name: a host's own, as OpenRouter's or
/// Ollama's, else its maker's, as OpenAI's, Mistral's, Moonshot AI's Kimi or Alibaba's Qwen.
fn provider_logo(name: &str) -> argonauts_core::look::logos::Logo {
    use argonauts_core::look::display_names::{maker, maker_of};

    let lower = name.to_lowercase();
    let logo = logo_for(&lower, None);
    if logo.mark.is_some() {
        return logo;
    }
    let word = lower.split_whitespace().next().unwrap_or_default();
    let mark = maker(word)
        .or_else(|| maker_of(word))
        .and_then(|maker| maker.logo);
    argonauts_core::look::logos::Logo { mark, ..logo }
}

/// Said of a CLI that keeps providers of its own and has none: what connects one.
pub fn no_provider(command: &str) -> String {
    format!("no provider set up · {command} auth login")
}

/// A checked version reads as fine, a new major one as worth a look, and one that needs an
/// update as wrong.
fn standing_role(standing: VersionStanding) -> Role {
    match standing {
        VersionStanding::Checked => Role::Success,
        VersionStanding::Newer => Role::Text,
        VersionStanding::NewMajor => Role::Warn,
        VersionStanding::Older | VersionStanding::Broken => Role::Danger,
        VersionStanding::Unknown => Role::Dim,
    }
}

/// Said of a CLI that asked about the folder before it showed anything else. Argonauts Code
/// never answers that for you.
pub fn untrusted_folder(command: &str) -> String {
    format!("{command} hasn't trusted this folder; start {command} here once")
}

/// `○ hi · Oct 4 · from an older Argonauts, can't be resumed · a archives`: a run by its
/// name, the day it last did something and the one word for its state. A run another build
/// wrote names the key that moves it out of the way.
fn run_row(kit: &Kit<'_>, run: &RunSummary, day: &str) -> Vec<Span<'static>> {
    let name = run.name.clone().unwrap_or_else(|| run.title.clone());
    let mark = if run.live { "●" } else { "○" };
    let mut row = vec![
        kit.span(
            format!("{mark} "),
            if run.live { Role::Question } else { Role::Dim },
        ),
        kit.span(name, Role::Text),
        kit.dim(format!(" · {day}")),
    ];
    if !run.stopped_at.is_empty() {
        row.push(kit.dim(format!(" · {}", run.stopped_at)));
    }
    if run.needs_you() {
        row.push(kit.span(" · waiting for you", Role::Warn));
    }
    if run.state == RunState::FromAnotherBuild {
        row.push(kit.dim(" · a archives"));
    }
    row
}

/// `Oct 4`.
fn day(at: OffsetDateTime) -> String {
    let month = at.month().to_string();
    format!("{} {}", &month[..3], at.day())
}

/// What a layer of the settings is, in words.
fn layer_words(name: &str) -> &'static str {
    match name {
        layer::GLOBAL => "your settings",
        layer::PROJECT => "this folder, shared",
        layer::PROJECT_LOCAL => "this folder, only you",
        layer::WORKFLOW => "its workflow",
        layer::FLAG => "this start only",
        layer::SESSION => "changed in this session",
        _ => "",
    }
}

/// `text` and spaces to `width` cells, with at least one space after it.
fn padded(text: &str, width: usize) -> String {
    let used = str_width(text);
    format!("{text}{}", " ".repeat(width.saturating_sub(used).max(1)))
}

/// The tab as plain lines, for a screen reader. The same facts, without the cards' frames.
pub fn spoken(view: &StatusTabView, names: &DisplayNames) -> Vec<String> {
    let mut lines = vec![format!("version: Argonauts Code {}", view.version)];
    if let Some(conversation) = &view.conversation {
        lines.push(format!("conversation: {}", conversation.id));
    }
    let git = if view.folder.git { "git" } else { "no git" };
    lines.push(format!("folder: {}, {git}", view.folder.shown));
    if let Some(main) = &view.main {
        lines.push(format!("main session: {}", names.worker(&main.worker)));
    }
    if let Some(cli) = &view.in_cli {
        lines.push(format!("in {}: {}, o opens it", cli.cli, cli.command));
    }
    lines.push(format!("workflow: {}", view.workflow.name));
    lines.push(format!("mode: {}", view.mode));
    for cli in &view.clis {
        let state =
            if cli.installed {
                let mut parts = vec![cli.version.clone().unwrap_or_default()];
                parts.extend(cli.account.clone());
                if cli.untrusted_folder {
                    parts.push(untrusted_folder(&cli.command));
                }
                parts.extend(cli.plan.clone());
                match &cli.providers {
                    Some(providers) if providers.is_empty() => {
                        parts.push(no_provider(&cli.command));
                    }
                    Some(providers) => parts.extend(providers.iter().map(|provider| {
                        format!("{} ({})", provider.name, provider.billing.words())
                    })),
                    None => {}
                }
                parts.extend(
                    cli.default_model
                        .as_ref()
                        .map(|model| format!("default {model}")),
                );
                parts.push(cli.check.state());
                if let Some(note) = cli.check.note() {
                    parts.push(match &note.command {
                        Some(command) => format!("{}, {command}", note.words),
                        None => note.words,
                    });
                }
                parts.join(", ")
            } else {
                "not installed".to_owned()
            };
        lines.push(format!("{}: {state}", cli.name));
    }
    for file in &view.settings.files {
        lines.push(format!("settings: {}, {} keys", file.path, file.keys));
    }
    match view.runs.len() {
        0 => lines.push(format!("runs: {NO_RUNS}")),
        n => lines.push(format!("runs: {n} in this folder")),
    }
    lines
}

#[cfg(test)]
mod tests;
