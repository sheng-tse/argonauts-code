use argonauts_core::deep_map::{DeepDone, DeepScope, DeepStart, MadeBy};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};

use super::*;
use crate::overview::golden::theme;
use crate::questions::deep_map::sample::page_offer;

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
        model: "ollama/qwen3-coder:30b-64k".parse().unwrap(),
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
        DeepKey::Ask(ViewRequest::DeepOffer)
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
fn d_with_no_deeper_map_asks_then_starts_with_the_answers() {
    let (flow, answer) = started();
    assert_eq!(
        answer,
        DeepKey::Send(UiCommand::DeepStart {
            start: DeepStart {
                scope: DeepScope::Everything,
                summarizer: "ollama/qwen3-coder:30b-64k".parse().unwrap(),
                download_embedder: true,
                redo: false,
            }
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
    let picture = include_str!("../../tests/golden/overview/choose_running.txt");
    // Before the first request is back, none of the scope's actions is done.
    let first = rows(flow.above_map(&kit).unwrap());
    assert_eq!(first[0], picture_rows(picture, 3..4)[0]);
    assert!(
        first[2].contains("Summarizing… 0 of 562 · esc"),
        "{}",
        first[2]
    );
    flow.sync(&running(312, 562, Some(50)));
    assert_eq!(
        rows(flow.above_map(&kit).unwrap()),
        picture_rows(picture, 3..6)
    );
    let under = rows(vec![flow.under_map(&kit).unwrap()]);
    let last = picture.lines().rev().find(|row| !row.trim().is_empty());
    assert_eq!(under[0], last.unwrap().trim_end());
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
fn d_on_the_label_map_switches_to_a_kept_deeper_one() {
    let mut flow = DeepFlow::default();
    assert_eq!(
        flow.d(Shown::Labels { kept: true }),
        DeepKey::Show(BehaviorMap::Deeper)
    );
}

#[test]
fn d_on_the_deeper_map_offers_its_new_actions_or_goes_back() {
    let mut offer = page_offer();
    offer.made_by = Some(made_by(37));
    let mut flow = DeepFlow::default();
    let shown = Shown::Deeper { unsummarized: 37 };
    assert_eq!(flow.d(shown), DeepKey::Ask(ViewRequest::DeepOffer));
    flow.accept(&offer);
    let theme = theme();
    let kit = Kit::new(&theme);
    let text = rows(flow.questions(&kit, usize::from(WIDTH)).unwrap());
    assert_eq!(text[0], "  ◆ Update the deeper map?");
    assert!(
        text.iter()
            .any(|row| row.contains("1. Update: 37 new actions, about 5 seconds")),
        "{text:#?}"
    );
    // The map's own summarizer is on this machine, so it adds them without asking again.
    assert_eq!(
        flow.key(KeyCode::Enter),
        Some(DeepKey::Send(UiCommand::DeepStart {
            start: DeepStart {
                scope: DeepScope::Everything,
                summarizer: "ollama/qwen3-coder:30b-64k".parse().unwrap(),
                download_embedder: false,
                redo: false,
            }
        }))
    );
    let lines = rows(flow.above_map(&kit).unwrap());
    assert!(lines[2].contains("0 of 37"), "{}", lines[2]);

    let mut flow = DeepFlow::default();
    flow.d(shown);
    flow.accept(&offer);
    flow.key(KeyCode::Down);
    assert_eq!(
        flow.key(KeyCode::Enter),
        Some(DeepKey::Show(BehaviorMap::Labels))
    );

    // With nothing new, d goes straight back to the labels.
    let mut flow = DeepFlow::default();
    assert_eq!(
        flow.d(Shown::Deeper { unsummarized: 0 }),
        DeepKey::Show(BehaviorMap::Labels)
    );
    assert!(!flow.asking());
}

#[test]
fn a_hosted_maps_update_asks_from_the_start() {
    let mut offer = page_offer();
    offer.made_by = Some(MadeBy {
        model: "claude-code/haiku".parse().unwrap(),
        name: "Haiku 5.5 on Claude Code".to_owned(),
        ..made_by(37)
    });
    let mut flow = DeepFlow::default();
    flow.d(Shown::Deeper { unsummarized: 37 });
    flow.accept(&offer);
    assert_eq!(flow.key(KeyCode::Enter), Some(DeepKey::Moved));
    let theme = theme();
    let kit = Kit::new(&theme);
    let text = rows(flow.questions(&kit, usize::from(WIDTH)).unwrap());
    assert_eq!(text[0], "  ◆ Summarize your agents' actions?");
}

#[test]
fn an_offer_nobody_asked_for_opens_nothing() {
    let mut flow = DeepFlow::default();
    flow.accept(&page_offer());
    assert!(!flow.asking());
}
