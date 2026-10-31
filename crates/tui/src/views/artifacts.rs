//! `/artifacts`: the pages this conversation published on claude.ai, newest first, as Claude
//! Code's own list shows the ones of a session. Enter or `o` opens one in the
//! browser, and `c` copies its link. Claude Code's list also has the artifacts of other
//! sessions and the ones shared with you, which only claude.ai can list.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::base::text::sanitize_line;
use crate::base::theme::{Role, Theme};
use crate::frame::state::{AppState, Published};

use super::super::base::draw::panes::{self, Footer, Header, Marks, Side};
use super::super::frame::view::list::{self, Column, ListState, fit_columns};
use crate::frame::view::{View, ViewCtx, ViewOutcome};

/// Cells between columns.
const GAP: usize = 2;

/// What the list says with nothing in it, in Claude Code's words.
pub const NONE_YET: &str = "No artifacts yet. Publish one with the Artifact tool.";

#[derive(Debug, Default)]
pub struct ArtifactsView {
    pub list: ListState,
    page: usize,
}

/// One row: a page, and who published it when an agent inside a run did.
struct Row {
    name: String,
    url: String,
    by: String,
}

impl ArtifactsView {
    pub fn new() -> Self {
        Self {
            list: ListState::default(),
            page: 1,
        }
    }

    /// Newest first, as read from the state on every frame, so one published while the list
    /// is open shows up in it.
    fn rows(state: &AppState) -> Vec<Row> {
        state
            .artifacts
            .all()
            .iter()
            .rev()
            .map(|published| Row {
                name: clean(&published.artifact.name),
                url: clean(&published.artifact.url),
                by: published_by(state, published),
            })
            .collect()
    }

    fn lines(&self, rows: &[Row], width: usize, theme: &Theme) -> Vec<Line<'static>> {
        let marks = Marks::for_theme(theme);
        let dim = theme.style(Role::Dim);
        // The selection mark, then the artifact mark, each with its space.
        let margin = 4;
        let cells = |f: &dyn Fn(&Row) -> String| rows.iter().map(f).collect::<Vec<_>>();
        let columns = fit_columns(
            vec![
                Column::new("name", cells(&|row| row.name.clone())),
                // The link is what the row is for, so it's cut last.
                Column::new("link", cells(&|row| row.url.clone())).flex(),
                Column::new("by", cells(&|row| row.by.clone())).keep(1),
            ],
            width.saturating_sub(margin),
            GAP,
        );
        rows.iter()
            .enumerate()
            .map(|(i, _)| {
                let selected = i == self.list.selected;
                let mut spans = vec![
                    if selected {
                        Span::styled(format!("{} ", marks.selected), theme.style(Role::Question))
                    } else {
                        Span::raw("  ")
                    },
                    Span::styled(format!("{} ", marks.artifact), theme.style(Role::Accent)),
                ];
                for (c, column) in columns.iter().enumerate() {
                    if c > 0 {
                        spans.push(Span::raw(" ".repeat(GAP)));
                    }
                    let style = match column.heading {
                        "name" if selected => theme.style(Role::Question),
                        "name" => theme.style(Role::Text),
                        _ => dim,
                    };
                    spans.push(Span::styled(column.cell(&column.cells[i]), style));
                }
                Line::from(spans)
            })
            .collect()
    }
}

/// `by build:overview` for a page an agent inside a run published. The main agent's say none.
fn published_by(state: &AppState, published: &Published) -> String {
    let Some((run, session)) = &published.by else {
        return String::new();
    };
    let label = state
        .session(run, session)
        .and_then(|session| session.label.clone())
        .unwrap_or_else(|| session.to_string());
    format!("by {}", clean(&label))
}

fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

impl View for ArtifactsView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &ViewCtx<'_>) {
        let theme = ctx.theme;
        let rows = Self::rows(ctx.state);
        let header = Header {
            title: Line::from(Span::styled("Artifacts", theme.style(Role::Label))),
            description: "The pages this conversation published on claude.ai".to_owned(),
            summary: panes::count(rows.len(), "artifact"),
        };
        let panes = panes::split(area, 0);
        panes::draw_header(buf, panes.header, &header, theme);
        panes::draw_box(buf, &panes, ["", ""], Side::Right, theme);

        let height = usize::from(panes.right.height);
        self.page = height.max(1);
        self.list.keep_visible(rows.len(), self.page);
        let lines = if rows.is_empty() {
            vec![Line::from(Span::styled(
                format!("  {NONE_YET}"),
                theme.style(Role::Dim),
            ))]
        } else {
            let lines = self.lines(&rows, usize::from(panes.right.width), theme);
            lines.into_iter().skip(self.list.offset).collect()
        };
        panes::put_lines(buf, panes.right, &lines);

        let marks = Marks::for_theme(theme);
        let mark = super::super::frame::view::pager::position_mark(
            self.list.offset,
            height,
            rows.len(),
            marks.down,
        );
        let hints = if rows.is_empty() {
            Footer::hints(&[(panes::ESC_BACK, 0)])
        } else {
            Footer::hints(&[
                ("↑↓  select", 0),
                ("⏎ or o open in the browser", 2),
                ("c copy its link", 1),
                (panes::ESC_BACK, 0),
            ])
        };
        panes::draw_footer(buf, panes.footer, hints, &mark, theme);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx<'_>) -> ViewOutcome {
        let key = list::standard(key, list::vim(ctx.state), KeyCode::Esc);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let rows = Self::rows(ctx.state);
        let selected = rows.get(self.list.selected);
        match (key.code, selected) {
            (KeyCode::Esc | KeyCode::Char('q'), _) if plain => return ViewOutcome::Close,
            (KeyCode::Enter | KeyCode::Char('o'), Some(row)) if plain => {
                return ViewOutcome::OpenLink {
                    url: row.url.clone(),
                    name: row.name.clone(),
                };
            }
            (KeyCode::Char('c'), Some(row)) if plain => return ViewOutcome::Copy(row.url.clone()),
            _ => {}
        }
        match list::nav_key(&key, self.page) {
            Some(nav) => {
                self.list.apply(nav, rows.len(), self.page);
                ViewOutcome::Consumed
            }
            None => ViewOutcome::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::AgentEvent;
    use argonauts_core::app::{AppEvent, ConversationEvent};
    use argonauts_core::base::names::TurnId;
    use serde_json::json;

    use super::*;
    use crate::base::theme::Glyphs;

    const CHART_PAGE: &str = "https://claude.ai/code/artifact/0b1e7c9a-4f1d-4c57-9a55-2f6f0d3a8e11";
    const REPORT: &str = "https://claude.ai/code/artifact/7d2c4b18-93aa-4e0f-8c61-5b0e9f1a2c33";

    /// A conversation whose main agent published the pages at `files`, in that order.
    fn published(files: &[(&str, &str)]) -> AppState {
        let turn = TurnId::numbered(1);
        let mut state = AppState::default();
        state.apply(&AppEvent::Conversation {
            event: ConversationEvent::TurnStarted {
                turn: turn.clone(),
                text: "write it up".into(),
                worker: "claude-code/opus".parse().unwrap(),
                read_only: false,
                commit: String::new(),
                uncommitted: Vec::new(),
                settings_sha256: None,
                continues: false,
                opens_a_talk: false,
            },
        });
        for (n, (file, url)) in files.iter().enumerate() {
            let id = format!("toolu_{n}");
            for event in [
                AgentEvent::ToolStarted {
                    id: id.clone(),
                    name: "Artifact".into(),
                    input: json!({"file_path": file}),
                    parent: None,
                },
                AgentEvent::ToolFinished {
                    id,
                    ok: true,
                    output: Some(format!("Published {file} at {url} (Version 1)")),
                    parent: None,
                },
            ] {
                state.apply(&AppEvent::Conversation {
                    event: ConversationEvent::Agent {
                        turn: turn.clone(),
                        event,
                    },
                });
            }
        }
        state
    }

    fn theme() -> Theme {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        theme
    }

    fn screen(view: &mut ArtifactsView, state: &AppState, width: u16) -> Vec<String> {
        let theme = theme();
        let ctx = ViewCtx {
            theme: &theme,
            state,
            now: Instant::now(),
        };
        let area = Rect::new(0, 0, width, 8);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &ctx);
        (0..area.height)
            .map(|y| {
                let row: String = (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect();
                row.trim_end().to_owned()
            })
            .collect()
    }

    fn press(view: &mut ArtifactsView, state: &AppState, code: KeyCode) -> ViewOutcome {
        let theme = theme();
        let ctx = ViewCtx {
            theme: &theme,
            state,
            now: Instant::now(),
        };
        view.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
    }

    #[test]
    fn the_list_names_each_page_with_its_link_newest_first() {
        let state = published(&[
            ("out/weekly-usage-chart.html", CHART_PAGE),
            ("out/test-report.html", REPORT),
        ]);
        let mut view = ArtifactsView::new();
        let wide = screen(&mut view, &state, 120).join("\n");
        assert!(wide.contains("Artifacts"), "{wide}");
        assert!(wide.contains("2 artifacts"), "{wide}");
        assert!(
            wide.contains(&format!("❯ ⧉ test-report         {REPORT}")),
            "{wide}"
        );
        assert!(
            wide.contains(&format!("  ⧉ weekly-usage-chart  {CHART_PAGE}")),
            "{wide}"
        );
        assert!(
            wide.contains("↑↓  select · ⏎ or o open in the browser · c copy its link · esc back"),
            "{wide}"
        );
        // A narrow screen cuts the link, never the name, and no row runs past the edge.
        let narrow = screen(&mut view, &state, 60);
        assert!(
            narrow
                .iter()
                .any(|row| row.contains("⧉ weekly-usage-chart  https://claude.ai/")),
            "{narrow:#?}"
        );
        assert!(
            narrow
                .iter()
                .all(|row| crate::base::text::str_width(row) <= 60),
            "{narrow:#?}"
        );
    }

    #[test]
    fn enter_or_o_opens_the_page_and_c_copies_its_link() {
        let state = published(&[
            ("out/weekly-usage-chart.html", CHART_PAGE),
            ("out/test-report.html", REPORT),
        ]);
        let mut view = ArtifactsView::new();
        screen(&mut view, &state, 100);
        for key in [KeyCode::Enter, KeyCode::Char('o')] {
            match press(&mut view, &state, key) {
                ViewOutcome::OpenLink { url, name } => {
                    assert_eq!((url.as_str(), name.as_str()), (REPORT, "test-report"));
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            press(&mut view, &state, KeyCode::Down),
            ViewOutcome::Consumed
        ));
        match press(&mut view, &state, KeyCode::Char('c')) {
            ViewOutcome::Copy(link) => assert_eq!(link, CHART_PAGE),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            press(&mut view, &state, KeyCode::Esc),
            ViewOutcome::Close
        ));
    }

    #[test]
    fn with_none_published_it_says_so_in_claude_codes_words() {
        let state = AppState::default();
        let mut view = ArtifactsView::new();
        let text = screen(&mut view, &state, 80).join("\n");
        assert!(
            text.contains("No artifacts yet. Publish one with the Artifact tool."),
            "{text}"
        );
        for key in [KeyCode::Enter, KeyCode::Char('o'), KeyCode::Char('c')] {
            assert!(matches!(
                press(&mut view, &state, key),
                ViewOutcome::Ignored | ViewOutcome::Consumed
            ));
        }
    }
}
