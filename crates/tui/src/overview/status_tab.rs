//! The Status tab: what runs here and how it is set up, for every CLI at once. The facts come
//! first as label and value, then a card for the CLIs, one for the settings in effect, and one
//! for the runs in this folder.

use argonauts_core::api::RunSummary;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::status_tab::{CliStatus, StatusTabView};
use argonauts_core::text::shown_name;
use ratatui::text::Span;

use super::cards::cli_role;
use crate::format::{self, WallClock};
use crate::text::{spans_width, str_width, truncate_spans};
use crate::theme::Role;
use crate::widgets::{Drawn, Frame, Header, Kit, spread};

/// The runs card when this folder has none.
pub const NO_RUNS: &str =
    "None yet. A workflow you start shows here, with what it's doing and anything waiting for you.";

/// The labels' column of the facts at the top.
const FACT_LABELS: usize = 16;
/// The columns of the CLIs card: the name, then the version.
const CLI_NAMES: usize = 19;
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
    /// The approval mode the mode line shows, which may be this session's own.
    pub mode: Option<&'a str>,
}

impl StatusTabCards<'_> {
    /// Version, conversation, folder, main session, workflow and mode, a row each.
    pub fn facts(&self, kit: &Kit<'_>, width: usize) -> Drawn {
        let view = self.view;
        let mut rows: Vec<(&str, Vec<Span<'static>>)> = vec![(
            "Version",
            vec![kit.span(format!("Argonauts Code {}", view.version), Role::Text)],
        )];
        if let Some(conversation) = &view.conversation {
            // As the panel's other tabs write a time of day: `7:12pm`.
            let started = super::usage::twelve_hour(self.clock.to_local(conversation.started));
            rows.push((
                "Conversation",
                vec![
                    kit.span(conversation.id.clone(), Role::Text),
                    kit.dim(format!(" · started {started}")),
                ],
            ));
        }
        let git = if view.folder.git {
            " · git"
        } else {
            " · no git, workflows keep a hidden history"
        };
        rows.push((
            "Folder",
            vec![
                kit.span(view.folder.shown.clone(), Role::Text),
                kit.dim(git),
            ],
        ));
        if let Some(main) = &view.main {
            let mut tail = String::new();
            if let Some(effort) = &main.effort {
                tail.push_str(&format!(" · {effort} effort"));
            }
            match (main.context_used, main.context_window) {
                (Some(used), Some(window)) => tail.push_str(&format!(
                    " · {} of {} context",
                    format::tokens(used),
                    format::tokens(window)
                )),
                (Some(used), None) => {
                    tail.push_str(&format!(" · {} in context", format::tokens(used)))
                }
                _ => {}
            }
            rows.push((
                "Main session",
                vec![
                    kit.span(self.names.worker(&main.worker), Role::Text),
                    kit.dim(tail),
                ],
            ));
        }
        let ultracode = if view.workflow.ultracode { "on" } else { "off" };
        rows.push((
            "Workflow",
            vec![
                kit.span(shown_name(&view.workflow.name), Role::Text),
                kit.dim(format!(
                    " · ultracode {ultracode} · workflow size {}",
                    view.workflow.size
                )),
            ],
        ));
        let mode = self.mode.unwrap_or(&view.mode);
        rows.push(("Mode", vec![kit.span(shown_name(mode), Role::Text)]));
        let mut drawn = Drawn::new();
        for (label, value) in rows {
            let mut row = vec![Span::raw(" "), kit.dim(padded(label, FACT_LABELS))];
            row.extend(value);
            drawn.push(truncate_spans(&row, width));
        }
        drawn
    }

    /// `◆ your CLIs`: each CLI with its version, who it's signed in as, its plan, and whether
    /// that version was checked. The main session's CLI has the filled dot.
    pub fn clis(&self, kit: &Kit<'_>, frame: Frame) -> Drawn {
        let inner = frame.inner();
        let mut body = Drawn::new();
        body.push(heading(
            kit,
            "your CLIs",
            "who you're logged in as on each",
            inner,
        ));
        if self.view.clis.is_empty() {
            body.push(vec![
                kit.dim("Doctor hasn't checked the CLIs yet; /doctor checks them."),
            ]);
        }
        for cli in &self.view.clis {
            body.push(truncate_spans(&cli_row(kit, cli), inner));
        }
        frame.draw(kit, body)
    }

    /// `◆ settings in effect`: each file and how many keys it sets, the mods, the main
    /// session's tools and its MCP servers. Opened, every key with where it came from.
    pub fn settings(&self, kit: &Kit<'_>, frame: Frame, opened: bool) -> Drawn {
        let inner = frame.inner();
        let settings = &self.view.settings;
        let mut body = Drawn::new();
        let right = if opened {
            "esc closes the keys"
        } else {
            "enter shows each key and where it came from"
        };
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
            "none, since its CLI takes no tools from Argonauts".to_owned()
        } else {
            settings.tools.join(" · ")
        };
        row("tools for the main session".to_owned(), tools);
        let cli = self
            .view
            .main
            .as_ref()
            .map(|main| self.names.harness(&main.worker.worker))
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
        if opened {
            body.push(Vec::new());
            if settings.keys.is_empty() {
                body.push(vec![
                    kit.dim("No key is set anywhere; each one is built in."),
                ]);
            }
            for entry in &settings.keys {
                let from = settings
                    .files
                    .iter()
                    .find(|file| file.layer == entry.layer)
                    .map_or(entry.layer.as_str(), |file| file.path.as_str());
                let row = vec![
                    kit.span(padded(&entry.key, SETTING_NAMES), Role::Text),
                    kit.span(format::setting_value(&entry.value), Role::Bright),
                    kit.dim(format!(" · {from}")),
                ];
                body.push(truncate_spans(&row, inner));
            }
        }
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
            body.push(truncate_spans(&run_row(kit, run), inner));
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
    let left = Header::new(title, Role::Card).left(kit);
    let right = vec![kit.dim(about.to_owned())];
    if spans_width(&left) + spans_width(&right) + 2 > width {
        return truncate_spans(&left, width);
    }
    spread(left, right, width)
}

/// `● Claude Code      2.1.295   you@example.com · Max · checked`: the dot in the CLI's
/// colour, its name in bold, its version, who it's signed in as, then its plan and how its
/// version stands, dim.
fn cli_row(kit: &Kit<'_>, cli: &CliStatus) -> Vec<Span<'static>> {
    let dot = if cli.main { "●" } else { "○" };
    let name_room = CLI_NAMES - 2;
    let name = vec![
        kit.strong(cli.name.clone()),
        Span::raw(" ".repeat(name_room.saturating_sub(str_width(&cli.name)).max(1))),
    ];
    if !cli.installed {
        let mut row = vec![kit.dim(format!("{dot} "))];
        row.extend(name);
        row.push(Span::raw(padded("—", CLI_VERSIONS)));
        row.push(kit.dim("not installed"));
        row.push(kit.dim(" · /update can't add it"));
        return row;
    }
    let mut row = vec![kit.span(format!("{dot} "), cli_role(&cli.id))];
    row.extend(name);
    row.push(kit.span(
        padded(cli.version.as_deref().unwrap_or("?"), CLI_VERSIONS),
        Role::Text,
    ));
    let mut tail: Vec<String> = Vec::new();
    match &cli.account {
        Some(account) => row.push(kit.span(account.clone(), Role::Text)),
        None if cli.signed_out => {
            row.push(kit.span("not signed in · /login signs in", Role::Warning));
        }
        None => row.push(kit.dim(argonauts_core::status_tab::UNNAMED_ACCOUNT)),
    }
    tail.extend(cli.plan.clone());
    tail.push(cli.note.clone());
    row.push(kit.dim(format!(" · {}", tail.join(" · "))));
    row
}

fn run_row(kit: &Kit<'_>, run: &RunSummary) -> Vec<Span<'static>> {
    let name = run.name.clone().unwrap_or_else(|| run.title.clone());
    let mark = if run.live { "●" } else { "○" };
    let mut row = vec![
        kit.span(
            format!("{mark} "),
            if run.live { Role::Card } else { Role::Dim },
        ),
        kit.span(name, Role::Text),
        kit.dim(format!(" · {}", run.run)),
    ];
    if !run.stopped_at.is_empty() {
        row.push(kit.dim(format!(" · {}", run.stopped_at)));
    }
    if run.needs_you() {
        row.push(kit.span(" · waiting for you", Role::Warning));
    }
    row
}

/// What a layer of the settings is, in words.
fn layer_words(layer: &str) -> &'static str {
    match layer {
        "global" => "your settings",
        "project" => "this folder, shared",
        "project-local" => "this folder, only you",
        "workflow" => "its preset",
        "flag" => "this start only",
        "session" => "changed in this session",
        _ => "",
    }
}

/// `text` and spaces to `width` cells, with at least one space after it.
fn padded(text: &str, width: usize) -> String {
    let used = crate::text::str_width(text);
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
    lines.push(format!("workflow: {}", view.workflow.name));
    lines.push(format!("mode: {}", view.mode));
    for cli in &view.clis {
        let state = if cli.installed {
            let mut parts = vec![cli.version.clone().unwrap_or_default()];
            parts.extend(cli.account.clone());
            parts.extend(cli.plan.clone());
            parts.push(cli.note.clone());
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
#[path = "status_tab_tests.rs"]
mod tests;
