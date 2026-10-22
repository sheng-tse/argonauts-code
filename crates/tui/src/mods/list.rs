//! `/mods`: every mod in a list, the built-in cards first, and beside it the one under the
//! pointer drawn live with where its file is, where it shows, and where its numbers come from.
//! Enter opens a mod's page. A file that didn't load is listed last with what's wrong with it.

use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::input::{ModsAction, SlashCommand};
use argonauts_core::settings::mods::view::{FileProblem, ModRow, ModRowKind, ModsView};
use argonauts_core::settings::mods::{ModFile, ModReading, ModSource, builtin, span_words};
use argonauts_core::settings::ui::ModName;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::bottom_line::{self, LinePage};
use super::draw;
use super::page::ModPage;
use super::rows::{PageGlyphs, breadcrumb};
use crate::kit::{Kit, fit, spread};
use crate::text::{sanitize_line, spans_width, str_width, wrap_text};
use crate::theme::Role;
use crate::views::panes::{self, Footer, Side};
use crate::views::{View, ViewCtx, ViewOutcome};

/// The list's share of the screen, in cells.
const LIST_CELLS: u16 = 52;
/// Cells a mod's name takes in the list.
const NAME_CELLS: usize = 16;
/// Cells the small picture takes.
const PICTURE_CELLS: usize = 12;
/// Cells before a mod's name: the pointer, the mark, and a space each.
const MARGIN_CELLS: usize = 4;

#[derive(Debug)]
enum Page {
    Card(Box<ModPage>),
    Line(Box<LinePage>),
}

/// Cells a fact's name takes in the pane beside the list: `Numbers`, and the space after it.
const FACT_CELLS: usize = 9;

/// One fact about a mod, its name dim and its words wrapped under themselves.
fn fact_lines(kit: &Kit<'_>, label: &str, words: &str, width: usize) -> Vec<Line<'static>> {
    wrap_text(
        &sanitize_line(words),
        kit.style(Role::Text),
        width,
        &[kit.dim(format!("{label:<FACT_CELLS$}"))],
        &[Span::raw(" ".repeat(FACT_CELLS))],
    )
}

/// What the pointer is on: a mod, or a file that didn't load.
enum Entry<'a> {
    Mod(&'a ModRow),
    Problem(&'a FileProblem),
}

#[derive(Debug)]
pub struct ModsScreen {
    view: ModsView,
    selected: usize,
    page: Option<Page>,
}

impl ModsScreen {
    pub fn new(mut view: ModsView) -> Self {
        let open = view.open.take();
        let mut screen = Self {
            view,
            selected: 0,
            page: None,
        };
        if let Some(name) = open
            && let Some(at) = screen.view.rows.iter().position(|row| row.name == name)
        {
            screen.selected = at;
            screen.open_page();
        }
        screen
    }

    fn entries(&self) -> Vec<Entry<'_>> {
        self.view
            .rows
            .iter()
            .map(Entry::Mod)
            .chain(self.view.problems.iter().map(Entry::Problem))
            .collect()
    }

    fn count(&self) -> usize {
        self.view.rows.len() + self.view.problems.len()
    }

    /// Your own mods, which the bottom line's page offers on both sides: any mod can sit
    /// there as a few words.
    fn line_mods(&self) -> Vec<String> {
        self.view
            .rows
            .iter()
            .filter(|row| !builtin::is_builtin(&row.name))
            .filter(|row| matches!(&row.kind, ModRowKind::Card { .. }))
            .map(|row| row.name.clone())
            .collect()
    }

    fn line_page(&self, row: &ModRow) -> Option<LinePage> {
        let ModRowKind::Own { text, built_in, .. } = &row.kind else {
            return None;
        };
        (row.name == builtin::BOTTOM_LINE).then(|| {
            LinePage::new(
                text,
                built_in,
                row.origin,
                row.path.clone(),
                self.view.line.clone(),
                self.line_mods(),
            )
        })
    }

    /// Opens the page of the mod under the pointer. A mod with a page elsewhere says where.
    fn open_page(&mut self) -> ViewOutcome {
        let Some(row) = self.view.rows.get(self.selected) else {
            return ViewOutcome::Notice(
                "That file didn't load. Fix what's listed beside it, and it loads at once"
                    .to_owned(),
            );
        };
        if let Some(page) = ModPage::for_row(row) {
            self.page = Some(Page::Card(Box::new(page)));
        } else if let Some(page) = self.line_page(row) {
            self.page = Some(Page::Line(Box::new(page)));
        } else if row.name == builtin::STATS {
            // The stats mod's page is the Stats tab's own, where its card shows each change.
            return ViewOutcome::Send(UiCommand::Slash {
                command: SlashCommand::Mods {
                    action: ModsAction::Page {
                        the_mod: ModName::Stats,
                    },
                    scope: None,
                },
            });
        } else {
            return ViewOutcome::Notice(format!(
                "{} draws itself and has a page of its own: /{} opens it",
                row.name, row.name
            ));
        }
        ViewOutcome::Consumed
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.count().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    /// Whether every card's small picture fits beside its places and origin in `width`. The
    /// pane beside the list draws the card anyway, so on a narrow list the pictures give way
    /// for the words, all together so the columns stay in line.
    fn pictures_fit(&self, width: usize) -> bool {
        self.view.rows.iter().all(|row| match &row.kind {
            ModRowKind::Card { file, .. } => {
                let words = str_width(&places_text(file)) + 1 + str_width(row.origin.word());
                MARGIN_CELLS + NAME_CELLS + 1 + PICTURE_CELLS + 2 + words <= width
            }
            ModRowKind::Own { .. } => true,
        })
    }

    /// One line of the list. `pictures` says whether the cards' small pictures fit.
    fn row_line(
        &self,
        kit: &Kit<'_>,
        row: &ModRow,
        selected: bool,
        width: usize,
        pictures: bool,
    ) -> Line<'static> {
        let pointer = if selected {
            PageGlyphs::for_kit(kit).pointer
        } else {
            " "
        };
        let name_style = if selected {
            kit.bold(Role::Question)
        } else {
            kit.bold(Role::Bright)
        };
        let mut left = vec![
            Span::styled(format!("{pointer} "), kit.bold(Role::Question)),
            kit.span(kit.glyphs.the_mod, Role::Accent),
            Span::raw(" "),
        ];
        let mut places_role = Role::Dim;
        let (title, picture, places) = match &row.kind {
            ModRowKind::Card {
                file,
                reading,
                hidden_by_line,
                ..
            } => {
                if *hidden_by_line {
                    places_role = Role::Warn;
                }
                // Only real numbers get a picture here. The pane beside it draws sample ones
                // and says so.
                let picture = pictures.then(|| {
                    reading
                        .as_ref()
                        .map(|reading| draw::small(kit, file, reading))
                        .unwrap_or_default()
                });
                (file.title.clone(), picture, places_text(file))
            }
            ModRowKind::Own { .. } => (row.name.replace('-', " "), None, "its own page".to_owned()),
        };
        left.extend(fit(
            vec![Span::styled(sanitize_line(&title).into_owned(), name_style)],
            NAME_CELLS,
        ));
        left.push(Span::raw(" "));
        // A mod that draws itself has no small picture, so its words take that room.
        if let Some(picture) = picture {
            left.extend(fit(picture, PICTURE_CELLS));
            left.push(Span::raw("  "));
        }
        left.push(kit.span(places, places_role));
        let origin = vec![kit.dim(row.origin.word())];
        let room = width.saturating_sub(spans_width(&origin) + 1);
        let left = if spans_width(&left) > room {
            crate::text::truncate_spans(&left, room)
        } else {
            left
        };
        Line::from(spread(left, origin, width))
    }

    fn list_lines(&self, kit: &Kit<'_>, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let pictures = self.pictures_fit(width);
        for (index, row) in self.view.rows.iter().enumerate() {
            lines.push(self.row_line(kit, row, index == self.selected, width, pictures));
        }
        if !self.view.problems.is_empty() {
            lines.push(Line::default());
            let count = self.view.problems.len();
            let heading = if count == 1 {
                "1 file didn't load".to_owned()
            } else {
                format!("{count} files didn't load")
            };
            lines.push(Line::from(kit.span(heading, Role::Warn)));
            for (index, problem) in self.view.problems.iter().enumerate() {
                let selected = self.view.rows.len() + index == self.selected;
                let pointer = if selected {
                    PageGlyphs::for_kit(kit).pointer
                } else {
                    " "
                };
                let name = problem.path.rsplit('/').next().unwrap_or(&problem.path);
                lines.push(Line::from(vec![
                    Span::styled(format!("{pointer} "), kit.bold(Role::Question)),
                    kit.span(format!("! {}", sanitize_line(name)), Role::Warn),
                ]));
            }
        }
        lines
    }

    /// The right pane for a card: its drawing, then the facts about it.
    fn card_lines(&self, kit: &Kit<'_>, row: &ModRow, width: usize) -> Vec<Line<'static>> {
        let ModRowKind::Card {
            file,
            reading,
            note,
            needs_trust,
            asks_for_line,
            hidden_by_line,
            ..
        } = &row.kind
        else {
            return Vec::new();
        };
        let sample;
        let numbers = match reading {
            Some(reading) => reading,
            None => {
                sample = ModReading::sample_for(file);
                &sample
            }
        };
        let mut lines = draw::preview(kit, file, numbers, width).lines;
        lines.push(Line::default());
        let mut fact = |label: &str, words: String| {
            lines.extend(fact_lines(kit, label, &words, width));
        };
        let shows = if !file.shown {
            format!("hidden; /{} shows it again", file.name)
        } else if file.places.is_empty() {
            "nowhere yet; its page ticks a place".to_owned()
        } else {
            let phrases: Vec<&str> = file.places.iter().map(|place| place.phrase()).collect();
            phrases.join(", ")
        };
        let numbers_from = match &file.data {
            ModSource::Builtin { source } => {
                format!("{} · {}", source.word(), source.describe())
            }
            ModSource::Command {
                command,
                every,
                timeout,
            } => format!(
                "{} · every {} · stopped after {}",
                sanitize_line(&command.join(" ")),
                span_words(*every),
                span_words(*timeout)
            ),
        };
        fact("File", row.path.clone());
        fact("Draws", file.body.describe().to_owned());
        fact("Shows", shows);
        if *asks_for_line {
            // The line shows what its own file lists, so a file that names it only asks.
            fact(
                "",
                "It asks for the bottom line: its page ticks that on".to_owned(),
            );
        }
        fact("Numbers", numbers_from);
        if *hidden_by_line {
            let why = format!(
                "The bottom line draws its own {} part by this name, so it never shows this \
                 file. Rename the file to put this mod there",
                row.name
            );
            lines.extend(wrap_text(&why, kit.style(Role::Warn), width, &[], &[]));
        }
        let (words, role) = if *needs_trust {
            (
                "The project's, and it runs a command: its page trusts it. Sample numbers \
                 until then"
                    .to_owned(),
                Role::Warn,
            )
        } else if reading.is_none() {
            let why = note.as_deref().unwrap_or("it has no numbers yet");
            (format!("Sample numbers: {why}"), Role::Dim)
        } else {
            (note.clone().unwrap_or_default(), Role::Dim)
        };
        if !words.is_empty() {
            lines.extend(wrap_text(&words, kit.style(role), width, &[], &[]));
        }
        lines
    }

    fn detail_lines(&self, kit: &Kit<'_>, width: usize) -> Vec<Line<'static>> {
        match self.entries().get(self.selected) {
            Some(Entry::Mod(row)) => match &row.kind {
                ModRowKind::Card { .. } => self.card_lines(kit, row, width),
                ModRowKind::Own { about, .. } => {
                    let mut lines = Vec::new();
                    if let Some(page) = self.line_page(row) {
                        lines.extend(
                            bottom_line::preview(kit, &page.file, &self.view.line, width).lines,
                        );
                        lines.push(Line::default());
                    }
                    let mut about = about.clone();
                    if let Some(first) = about.get(..1) {
                        about = format!("{}{}", first.to_uppercase(), &about[1..]);
                    }
                    lines.extend(wrap_text(&about, kit.style(Role::Text), width, &[], &[]));
                    lines.extend(fact_lines(kit, "File", &row.path, width));
                    if row.name != builtin::BOTTOM_LINE {
                        lines.push(Line::from(
                            kit.dim(format!("It draws itself. /{} opens it", row.name)),
                        ));
                    }
                    lines
                }
            },
            Some(Entry::Problem(problem)) => {
                let mut lines = vec![
                    Line::from(kit.span(sanitize_line(&problem.path).into_owned(), Role::Warn)),
                    Line::from(kit.dim("It didn't load, and the rest work without it. Fix:")),
                ];
                for why in &problem.problems {
                    lines.extend(wrap_text(
                        &sanitize_line(why),
                        kit.style(Role::Text),
                        width,
                        &[Span::raw("· ")],
                        &[Span::raw("  ")],
                    ));
                }
                lines
            }
            None => vec![Line::from(kit.dim("No mods"))],
        }
    }
}

impl View for ModsScreen {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        match &self.page {
            Some(Page::Card(page)) => return page.render(area, buf, ctx.theme),
            Some(Page::Line(page)) => return page.render(area, buf, ctx.theme),
            None => {}
        }
        let theme = ctx.theme;
        let kit = Kit::new(theme);
        let panes = panes::split(area, LIST_CELLS);
        let description = "Every mod, where it shows, and what it draws now. A mod is a file";
        // The description is said whole; the folders beside it shorten to yours, then go.
        let room = usize::from(area.width).saturating_sub(str_width(description) + 2);
        let both = match &self.view.project_dir {
            Some(project) => format!("{} · {project}", self.view.yours_dir),
            None => self.view.yours_dir.clone(),
        };
        let dirs = [both, self.view.yours_dir.clone()]
            .into_iter()
            .find(|dirs| str_width(dirs) <= room)
            .unwrap_or_default();
        let header = panes::Header {
            title: Line::from(breadcrumb(&kit, &["Config", "Mods"])),
            description: description.to_owned(),
            summary: dirs,
        };
        panes::draw_header(buf, panes.header, &header, theme);
        let name = match self.entries().get(self.selected) {
            Some(Entry::Mod(row)) => match &row.kind {
                ModRowKind::Card { file, .. } => sanitize_line(&file.title).into_owned(),
                ModRowKind::Own { .. } => row.name.replace('-', " "),
            },
            Some(Entry::Problem(_)) => "didn't load".to_owned(),
            None => String::new(),
        };
        panes::draw_box(buf, &panes, ["Mods", &name], Side::Left, theme);
        let list = self.list_lines(&kit, usize::from(panes.left.width));
        // The pointer's line stays in view on a short screen.
        let height = usize::from(panes.left.height);
        let at = if self.selected < self.view.rows.len() {
            self.selected
        } else {
            self.selected + 2
        };
        let offset = (at + 1).saturating_sub(height);
        let shown: Vec<Line<'static>> = list.into_iter().skip(offset).collect();
        panes::put_lines(buf, panes.left, &shown);
        let detail = self.detail_lines(&kit, usize::from(panes.right.width));
        panes::put_lines(buf, panes.right, &detail);
        let mark = panes::count(self.view.rows.len(), "mod");
        let hints = Footer::hints(&[("↑↓  mod", 0), ("⏎ its page", 0), (panes::ESC_BACK, 0)]);
        panes::draw_footer(buf, panes.footer, hints, &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let outcome = match &mut self.page {
            Some(Page::Card(page)) => Some(page.handle_key(key)),
            Some(Page::Line(page)) => Some(page.handle_key(key, ctx.theme)),
            None => None,
        };
        match outcome {
            // A page goes back to the list, and the list back to the conversation.
            Some(ViewOutcome::Close) => {
                self.page = None;
                return ViewOutcome::Consumed;
            }
            Some(outcome) => return outcome,
            None => {}
        }
        match key.code {
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::PageUp | KeyCode::Home => self.move_by(-(self.count() as isize)),
            KeyCode::PageDown | KeyCode::End => self.move_by(self.count() as isize),
            KeyCode::Enter | KeyCode::Right => return self.open_page(),
            // On the way back the conversation asks for the mods once more, since what a page saved
            // came to this screen and not to the one behind it.
            KeyCode::Esc | KeyCode::Left => {
                return ViewOutcome::SendToConversation(UiCommand::Request {
                    view: ViewRequest::Mods {
                        open: None,
                        refresh: true,
                    },
                });
            }
            _ => return ViewOutcome::Ignored,
        }
        ViewOutcome::Consumed
    }

    /// Newer files and numbers. The pointer stays on its mod, and an open page keeps what's
    /// unsaved.
    fn accept(&mut self, data: &ViewData) -> bool {
        let ViewData::Mods { view } = data else {
            return false;
        };
        let pointed = self
            .view
            .rows
            .get(self.selected)
            .map(|row| row.name.clone());
        self.view = (**view).clone();
        self.view.open = None;
        if let Some(name) = pointed
            && let Some(at) = self.view.rows.iter().position(|row| row.name == name)
        {
            self.selected = at;
        }
        self.selected = self.selected.min(self.count().saturating_sub(1));
        match &mut self.page {
            Some(Page::Card(page)) => match self.view.row(page.name()) {
                Some(row) => page.refresh(row),
                // Its file went away while its page was open.
                None => self.page = None,
            },
            Some(Page::Line(page)) => {
                if let Some(row) = self.view.row(builtin::BOTTOM_LINE)
                    && let ModRowKind::Own { text, .. } = &row.kind
                {
                    page.refresh(text, row.origin, self.view.line.clone());
                }
            }
            None => {}
        }
        true
    }
}

/// Where a card shows, for its row: the first place and how many more. The pane beside the
/// list names them all.
fn places_text(file: &ModFile) -> String {
    match file.places.as_slice() {
        _ if !file.shown => "hidden".to_owned(),
        [] => "nowhere yet".to_owned(),
        [only] => only.word().to_owned(),
        [first, more @ ..] => format!("{} +{}", first.word(), more.len()),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::mods::{ModOrigin, check};
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::state::AppState;
    use crate::theme::{ColorLevel, Theme};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    pub(crate) fn sample_view() -> ModsView {
        let mut rows = Vec::new();
        for built in builtin::BUILTIN {
            let kind = match built.kind {
                builtin::BuiltinKind::Card => {
                    let file = check::read(built.name, built.text).unwrap();
                    ModRowKind::Card {
                        built_in: Some(Box::new(file.clone())),
                        reading: Some(ModReading::sample(file.body)),
                        file: Box::new(file),
                        note: None,
                        needs_trust: false,
                        asks_for_line: false,
                        hidden_by_line: false,
                    }
                }
                builtin::BuiltinKind::Own { about } => ModRowKind::Own {
                    about: about.to_owned(),
                    text: built.text.to_owned(),
                    built_in: built.text.to_owned(),
                },
            };
            rows.push(ModRow {
                name: built.name.to_owned(),
                origin: ModOrigin::BuiltIn,
                path: format!("~/.argonauts/mods/{}.toml", built.name),
                kind,
            });
        }
        let (name, text) = builtin::STARTERS
            .iter()
            .find(|(name, _)| *name == "checks")
            .unwrap();
        rows.push(ModRow {
            name: (*name).to_owned(),
            origin: ModOrigin::Yours,
            path: "~/.argonauts/mods/checks.toml".to_owned(),
            kind: ModRowKind::Card {
                file: Box::new(check::read(name, text).unwrap()),
                built_in: None,
                reading: None,
                note: Some("./checks.sh isn't there yet".to_owned()),
                needs_trust: false,
                asks_for_line: false,
                hidden_by_line: false,
            },
        });
        ModsView {
            rows,
            problems: vec![FileProblem {
                path: "~/.argonauts/mods/broken.toml".to_owned(),
                problems: vec!["body is \"pie\"; use stack, meters or list".to_owned()],
            }],
            yours_dir: "~/.argonauts/mods".to_owned(),
            project_dir: Some(".argonauts/mods".to_owned()),
            ..ModsView::default()
        }
    }

    fn screen_text(screen: &mut ModsScreen, width: u16, height: u16) -> String {
        let theme = Theme::dark().with_level(ColorLevel::None);
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: std::time::Instant::now(),
        };
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        screen.render(area, &mut buf, &ctx);
        (0..height)
            .map(|y| {
                let row: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
                row.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(screen: &mut ModsScreen, code: KeyCode) -> ViewOutcome {
        let theme = Theme::dark();
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: std::time::Instant::now(),
        };
        screen.handle_key(key(code), &ctx)
    }

    #[test]
    fn the_list_names_every_mod_and_draws_the_one_under_the_pointer() {
        let mut screen = ModsScreen::new(sample_view());
        let text = screen_text(&mut screen, 120, 30);
        for name in [
            "context",
            "plan usage",
            "spend",
            "tokens",
            "workflows",
            "waiting for you",
            "stats",
            "bottom line",
            "checks",
        ] {
            assert!(text.contains(name), "{name}\n{text}");
        }
        assert!(text.contains("1 file didn't load"), "{text}");
        assert!(text.contains("! broken.toml"), "{text}");
        // The context card, drawn, with where its file would be.
        assert!(text.contains("◆ context"), "{text}");
        assert!(
            text.contains("File     ~/.argonauts/mods/context.toml"),
            "{text}"
        );
        assert!(text.contains("Shows    on the Context tab"), "{text}");
    }

    #[test]
    fn a_row_names_a_mod_s_first_place_and_counts_the_rest() {
        let mut view = sample_view();
        if let ModRowKind::Card { file, .. } = &mut view.rows[1].kind {
            file.places
                .push(argonauts_core::settings::mods::ModPlace::BottomLine);
        }
        let mut screen = ModsScreen::new(view);
        screen.selected = 1;
        let text = screen_text(&mut screen, 120, 30);
        let row = text.lines().find(|line| line.contains("❯ ◆ plan usage"));
        assert!(row.unwrap().contains("usage +1 "), "{text}");
        assert!(
            text.contains("Shows    on the Usage tab, on the bottom line"),
            "{text}"
        );
    }

    #[test]
    fn enter_opens_a_mod_s_page_and_esc_comes_back_to_the_list() {
        let mut screen = ModsScreen::new(sample_view());
        assert!(matches!(
            press(&mut screen, KeyCode::Enter),
            ViewOutcome::Consumed
        ));
        let text = screen_text(&mut screen, 120, 30);
        assert!(text.contains("Config › Mods › context"), "{text}");
        // A built-in card's page offers where it shows.
        assert!(text.contains("Places"), "{text}");
        assert!(matches!(
            press(&mut screen, KeyCode::Esc),
            ViewOutcome::Consumed
        ));
        assert!(screen.page.is_none());
        assert!(matches!(
            press(&mut screen, KeyCode::Esc),
            ViewOutcome::SendToConversation(UiCommand::Request {
                view: ViewRequest::Mods { refresh: true, .. }
            })
        ));
    }

    #[test]
    fn mods_with_a_name_opens_that_page_at_once() {
        let mut view = sample_view();
        view.open = Some("bottom-line".to_owned());
        let mut screen = ModsScreen::new(view);
        let text = screen_text(&mut screen, 120, 34);
        assert!(text.contains("Config › Mods › bottom line"), "{text}");
        assert!(text.contains("Gives up first"), "{text}");
        assert!(matches!(screen.page, Some(Page::Line(_))));
    }

    #[test]
    fn a_file_that_asks_for_the_bottom_line_says_how_to_get_there() {
        let mut view = sample_view();
        let at = view.rows.len() - 1;
        if let ModRowKind::Card {
            file,
            asks_for_line,
            ..
        } = &mut view.rows[at].kind
        {
            file.places.clear();
            *asks_for_line = true;
        }
        let mut screen = ModsScreen::new(view);
        screen.selected = at;
        let text = screen_text(&mut screen, 120, 30);
        assert!(
            text.contains("Shows    nowhere yet; its page ticks a place"),
            "{text}"
        );
        assert!(
            text.contains("It asks for the bottom line: its page ticks that on"),
            "{text}"
        );
    }

    #[test]
    fn a_mod_the_bottom_line_hides_says_why_and_how_to_show_it() {
        let mut view = sample_view();
        let at = view.rows.len() - 1;
        view.rows[at].name = "artifact".to_owned();
        if let ModRowKind::Card {
            file,
            hidden_by_line,
            ..
        } = &mut view.rows[at].kind
        {
            file.places = vec![argonauts_core::settings::mods::ModPlace::BottomLine];
            *hidden_by_line = true;
        }
        let mut screen = ModsScreen::new(view);
        screen.selected = at;
        let text = screen_text(&mut screen, 120, 34);
        assert!(
            text.contains("The bottom line draws its own artifact part by this name, so"),
            "{text}"
        );
        assert!(text.contains("Rename the file to put this mod"), "{text}");
        // The row's place is in the warning colour.
        let theme = Theme::dark();
        let kit = Kit::new(&theme);
        let line = screen.row_line(&kit, &screen.view.rows[at], true, 120, true);
        let place = line.spans.iter().find(|span| span.content == "bottom-line");
        assert_eq!(place.unwrap().style, kit.style(Role::Warn));
    }

    #[test]
    fn at_100_columns_the_words_stay_whole_and_the_pictures_give_way() {
        let mut view = sample_view();
        if let ModRowKind::Card { file, .. } = &mut view.rows[1].kind {
            file.places
                .push(argonauts_core::settings::mods::ModPlace::BottomLine);
        }
        let mut screen = ModsScreen::new(view);
        let text = screen_text(&mut screen, 100, 30);
        assert!(
            text.contains("Every mod, where it shows, and what it draws now. A mod is a file"),
            "{text}"
        );
        assert!(text.contains("~/.argonauts/mods"), "{text}");
        for (name, place) in [
            ("context", "context"),
            ("plan usage", "usage +1"),
            ("waiting for you", "status"),
            ("stats", "its own page"),
            ("checks", "above"),
        ] {
            let row = text
                .lines()
                .find(|line| line.contains(&format!("◆ {name} ")))
                .unwrap_or_else(|| panic!("{name}\n{text}"));
            assert!(row.contains(&format!(" {place} ")), "{name}: {row}\n{text}");
        }
        assert!(!text.contains('…'), "{text}");
        // The pictures come back where there's room for them.
        let wide = screen_text(&mut screen, 120, 30);
        assert!(wide.contains("◆ context          ━━━━"), "{wide}");
    }

    #[test]
    fn the_bottom_line_s_page_offers_every_mod_of_yours_on_both_sides() {
        let mut view = sample_view();
        view.open = Some("bottom-line".to_owned());
        let mut screen = ModsScreen::new(view);
        let text = screen_text(&mut screen, 120, 34);
        let row = |label: &str| {
            let found = text
                .lines()
                .find(|line| line.trim_start().starts_with(label));
            found
                .unwrap_or_else(|| panic!("{label}\n{text}"))
                .to_owned()
        };
        // On the left it is one of the line's own offers. The right takes it as yours.
        assert_eq!(row("❯ Left").matches("☐ checks").count(), 1, "{text}");
        assert!(row("Right").contains("☐ checks"), "{text}");
        // A built-in card isn't offered where the line has no part for it.
        assert!(!row("Right").contains("waiting"), "{text}");
    }

    #[test]
    fn stats_says_where_its_own_page_is_and_a_broken_file_what_to_fix() {
        let mut screen = ModsScreen::new(sample_view());
        screen.selected = screen
            .view
            .rows
            .iter()
            .position(|row| row.name == "stats")
            .unwrap();
        // The stats mod's page opens over the Stats tab.
        match press(&mut screen, KeyCode::Enter) {
            ViewOutcome::Send(UiCommand::Slash {
                command:
                    SlashCommand::Mods {
                        action:
                            ModsAction::Page {
                                the_mod: ModName::Stats,
                            },
                        scope: None,
                    },
            }) => {}
            other => panic!("{other:?}"),
        }
        press(&mut screen, KeyCode::End);
        let text = screen_text(&mut screen, 120, 30);
        assert!(
            text.contains("It didn't load, and the rest work without it"),
            "{text}"
        );
        assert!(text.contains("· body is \"pie\""), "{text}");
        assert!(matches!(
            press(&mut screen, KeyCode::Enter),
            ViewOutcome::Notice(_)
        ));
    }

    #[test]
    fn a_refresh_keeps_the_pointer_on_its_mod() {
        let mut screen = ModsScreen::new(sample_view());
        screen.selected = screen.view.rows.len() - 1;
        let mut fresh = sample_view();
        fresh.rows.remove(0);
        fresh.refresh = true;
        assert!(screen.accept(&ViewData::Mods {
            view: Box::new(fresh)
        }));
        assert_eq!(screen.view.rows[screen.selected].name, "checks");
    }

    #[test]
    fn the_ascii_set_draws_none_of_the_screens_own_unicode_marks() {
        let mut theme = Theme::dark();
        theme.glyphs = crate::theme::Glyphs::ASCII;
        let state = AppState::default();
        let ctx = ViewCtx {
            theme: &theme,
            state: &state,
            now: std::time::Instant::now(),
        };
        let drawn = |screen: &mut ModsScreen| -> String {
            let area = Rect::new(0, 0, 120, 34);
            let mut buf = Buffer::empty(area);
            screen.render(area, &mut buf, &ctx);
            buf.content().iter().map(|cell| cell.symbol()).collect()
        };
        let mut pages = vec![drawn(&mut ModsScreen::new(sample_view()))];
        for name in ["checks", "bottom-line"] {
            let mut view = sample_view();
            view.open = Some(name.to_owned());
            pages.push(drawn(&mut ModsScreen::new(view)));
        }
        for page in pages {
            for mark in ["❯", "⏵", "●", "○", "■", "☒", "☐", "›", "◆", "╭"] {
                assert!(!page.contains(mark), "{mark}");
            }
        }
    }

    #[test]
    fn the_screen_draws_at_any_size() {
        let mut screen = ModsScreen::new(sample_view());
        for (width, height) in [(200, 60), (120, 30), (80, 24), (40, 10), (12, 4), (2, 1)] {
            screen_text(&mut screen, width, height);
            screen.selected = screen.count() - 1;
            screen_text(&mut screen, width, height);
            screen.selected = 0;
        }
    }
}
