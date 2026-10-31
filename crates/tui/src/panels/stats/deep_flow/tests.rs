use argonauts_core::behaviors::deep_map::{DeepDone, DeepScope, DeepStart, MadeBy};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};

use super::*;
use crate::panels::stats::deep_map::sample::page_offer;
use crate::testing::pictures::golden::theme;

const WIDTH: u16 = 118;

fn rows(lines: Vec<Line<'static>>) -> Vec<String> {
    let area = Rect::new(0, 0, WIDTH, lines.len() as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines).render(area, &mut buf);
    (0..area.height)
        .map(|y| {
            let row: String = (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect();
            row.trim_end().to_owned()
        })
        .collect()
}

fn picture_rows(text: &str, rows: std::ops::Range<usize>) -> Vec<String> {
    text.lines()
        .skip(rows.start)
        .take(rows.len())
        .map(|row| row.trim_end().to_owned())
        .collect()
}

fn made_by(new_actions: usize) -> MadeBy {
    MadeBy {
        model: "ollama/qwen3-coder:30b".parse().unwrap(),
        name: "Qwen3 Coder 30B on Ollama".to_owned(),
        actions: 562,
        new_actions,
        prompt_changed: false,
    }
}

fn running(done: usize, total: usize, seconds_left: Option<u64>) -> DeepUpdate {
    DeepUpdate {
        running: Some(DeepProgress {
            done,
            total,
            seconds_left,
            summarizer: "Qwen3 Coder 30B on Ollama".to_owned(),
            short: "Qwen3 Coder 30B".to_owned(),
            redo: false,
            downloading: false,
        }),
        ..DeepUpdate::default()
    }
}

/// `d` on the first visit's label map, answered with the first answer each time: everything,
/// the local Qwen, and the embedder's download.
fn started() -> (DeepFlow, DeepKey) {
    let mut flow = DeepFlow::default();
    assert_eq!(
        flow.d(Shown::Labels { kept: false }),
        DeepKey::Ask(ViewRequest::DeepOffer { start: None })
    );
    assert!(!flow.asking(), "the questions wait for the offer");
    flow.accept(&page_offer());
    assert!(flow.asking());
    assert!(flow.claims(KeyCode::Down));
    assert_eq!(flow.key(KeyCode::Enter), Some(DeepKey::Moved));
    assert_eq!(flow.key(KeyCode::Enter), Some(DeepKey::Moved));
    let answer = flow.key(KeyCode::Enter).unwrap();
    (flow, answer)
}

#[test]
fn d_with_no_summaries_asks_then_starts_with_the_answers() {
    let (flow, answer) = started();
    assert_eq!(
        answer,
        DeepKey::Send(UiCommand::DeepStart {
            start: DeepStart {
                scope: DeepScope::Everything,
                summarizer: "ollama/qwen3-coder:30b".parse().unwrap(),
                download_embedder: true,
                redo: false,
                download_summarizer: false,
            },
        })
    );
    assert!(!flow.asking());
    assert!(flow.running());
}

#[test]
fn while_it_runs_the_tab_says_who_summarizes_and_how_far_as_its_picture_shows() {
    let (mut flow, _) = started();
    let theme = theme();
    let kit = Kit::new(&theme);
    // The whole screen's picture, which the tab's own golden draws cell for cell.
    let picture = include_str!("../../../../tests/golden/behaviors/deep_running.txt");
    // Before the first request is back, none of the scope's actions is done, and the offer
    // says how long they take.
    let first = rows(flow.above_map(&kit).unwrap());
    assert_eq!(first[0], picture_rows(picture, 5..6)[0]);
    assert_eq!(
        first[2],
        "  ✻  Summarizing…  0 of 562 · about 2 min left · esc keeps it going in the background · \
         x stops it"
    );
    // The app's first word has no estimate yet, so the offer's says what's left of the rest.
    flow.sync(&running(281, 562, None));
    let half = rows(flow.above_map(&kit).unwrap());
    assert!(
        half[2].contains("Summarizing…  281 of 562 · about 1 min left · esc"),
        "{}",
        half[2]
    );
    flow.sync(&running(312, 562, Some(50)));
    assert_eq!(
        rows(flow.above_map(&kit).unwrap()),
        picture_rows(picture, 5..8)
    );
    let under = rows(vec![flow.under_map(&kit).unwrap()]);
    assert!(under[0].contains("The dots arrive as their summaries do"));
    assert!(
        picture.lines().any(|row| row.trim_end() == under[0]),
        "{}",
        under[0]
    );
}

#[test]
fn x_stops_it_and_esc_leaves_it_going_as_the_panel_closes() {
    let (mut flow, _) = started();
    assert!(flow.claims(KeyCode::Char('x')));
    assert!(!flow.claims(KeyCode::Esc));
    assert!(!flow.claims(KeyCode::Char('d')), "one update at a time");
    assert_eq!(
        flow.key(KeyCode::Char('x')),
        Some(DeepKey::Send(UiCommand::DeepStop))
    );
}

#[test]
fn an_update_the_app_started_after_a_run_shows_too() {
    let mut flow = DeepFlow::default();
    let theme = theme();
    let kit = Kit::new(&theme);
    assert!(flow.above_map(&kit).is_none());
    assert!(!flow.claims(KeyCode::Char('x')));
    flow.sync(&running(3, 37, None));
    let lines = rows(flow.above_map(&kit).unwrap());
    assert!(lines[2].contains("3 of 37"), "{}", lines[2]);
    assert!(flow.claims(KeyCode::Char('x')));
}

#[test]
fn once_it_ends_the_tab_is_the_map_again() {
    let (mut flow, _) = started();
    let mut update = running(562, 562, None);
    flow.sync(&update);
    update.done(&DeepDone::default());
    flow.sync(&update);
    assert!(!flow.running());
    assert!(flow.under_map(&Kit::new(&theme())).is_none());
    assert!(flow.claims(KeyCode::Char('d')));
}

#[test]
fn d_on_the_label_map_switches_to_kept_summaries() {
    let mut flow = DeepFlow::default();
    assert_eq!(
        flow.d(Shown::Labels { kept: true }),
        DeepKey::Show(BehaviorMap::Summaries)
    );
}

#[test]
fn d_on_summaries_only_goes_back_to_the_labels() {
    let mut flow = DeepFlow::default();
    for unsummarized in [0, 37] {
        assert_eq!(
            flow.d(Shown::Summaries { unsummarized }),
            DeepKey::Show(BehaviorMap::Labels)
        );
        assert!(!flow.asking());
    }
}

#[test]
fn u_has_the_app_add_the_new_actions_and_shows_it_started() {
    let mut flow = DeepFlow::default();
    let by = "Qwen3 Coder 30B on Ollama";
    assert_eq!(flow.u(Shown::Summaries { unsummarized: 0 }, by), None);
    assert_eq!(flow.u(Shown::Labels { kept: true }, by), None);
    assert_eq!(
        flow.u(Shown::Summaries { unsummarized: 37 }, by),
        Some(DeepKey::Send(UiCommand::DeepUpdate))
    );
    assert!(flow.running(), "x stops it from the first key");
    let theme = theme();
    let kit = Kit::new(&theme);
    let lines = rows(flow.above_map(&kit).unwrap());
    assert!(lines[2].contains("0 of 37"), "{}", lines[2]);
    // The app started it, so an offer that comes later for another tab opens nothing here.
    flow.sync(&running(3, 37, None));
    flow.accept(&page_offer());
    assert!(!flow.asking());
}

#[test]
fn u_asks_only_when_the_app_sends_the_offer_back() {
    let mut offer = page_offer();
    offer.made_by = Some(MadeBy {
        model: "claude-code/haiku".parse().unwrap(),
        name: "Haiku 5.5 on Claude Code".to_owned(),
        ..made_by(37)
    });
    let mut flow = DeepFlow::default();
    flow.u(Shown::Summaries { unsummarized: 37 }, "Haiku 5.5");
    flow.accept(&offer);
    assert!(flow.asking());
    assert!(!flow.running(), "nothing started while it asks");
    assert_eq!(
        flow.key(KeyCode::Esc),
        Some(DeepKey::Moved),
        "Esc keeps Summaries"
    );
    assert!(!flow.asking());
}

#[test]
fn an_update_that_failed_says_so_under_the_headline_until_you_leave() {
    let mut flow = DeepFlow::default();
    let mut update = running(40, 823, None);
    let progress = update.running.as_mut().unwrap();
    progress.summarizer = "Haiku 5.5 on Claude Code".to_owned();
    progress.short = "Haiku 5.5".to_owned();
    flow.sync(&update);
    update.done(&DeepDone {
        written: 40,
        failed: Some("the fake stopped answering".to_owned()),
        placed: 650,
        ..DeepDone::default()
    });
    flow.sync(&update);
    let theme = theme();
    let kit = Kit::new(&theme);
    assert_eq!(
        rows(flow.kept_lines(&kit).unwrap()),
        [
            " Update stopped: Haiku 5.5 didn't answer after 40 of 823 summaries.",
            " The map is as it was, and the 40 are kept for the next try.",
        ]
    );
    flow.leave();
    assert!(flow.kept_lines(&kit).is_none());
    // The panel opened again after it ended didn't see it.
    let mut reopened = DeepFlow::default();
    reopened.sync(&update);
    assert!(reopened.kept_lines(&kit).is_none());
    // An update that placed new dots made a new map, so there's nothing to say.
    let mut flow = DeepFlow::default();
    let mut update = running(0, 9, None);
    flow.sync(&update);
    update.done(&DeepDone {
        written: 9,
        new_places: 9,
        ..DeepDone::default()
    });
    flow.sync(&update);
    assert!(flow.kept_lines(&kit).is_none());
}

#[test]
fn an_offer_nobody_asked_for_opens_nothing() {
    let mut flow = DeepFlow::default();
    flow.accept(&page_offer());
    assert!(!flow.asking());
}

#[test]
fn starting_a_stopped_ollama_asks_again_and_reopens_open_models() {
    use crate::panels::stats::deep_open::sample::{open_offer, stopped_offer};

    let mut flow = DeepFlow::default();
    flow.d(Shown::Labels { kept: false });
    flow.accept(&stopped_offer());
    for _ in 0..2 {
        assert_eq!(flow.key(KeyCode::Enter), Some(DeepKey::Moved));
    }
    assert_eq!(
        flow.key(KeyCode::Enter),
        Some(DeepKey::Ask(ViewRequest::DeepOffer {
            start: Some("ollama".to_owned())
        }))
    );
    assert!(flow.asking(), "the questions wait for the server");
    assert_eq!(flow.key(KeyCode::Enter), Some(DeepKey::Moved));
    flow.accept(&open_offer());
    let theme = crate::testing::pictures::golden::theme();
    let kit = crate::base::kit::Kit::new(&theme);
    let text: Vec<String> = flow
        .questions(&kit, 118)
        .unwrap()
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    assert!(text[0].contains("Open models"), "{text:#?}");
    assert!(
        text.iter()
            .any(|row| row.contains("Installed through Ollama"))
    );
}
