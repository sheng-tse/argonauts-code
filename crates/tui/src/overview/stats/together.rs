//! Every model, or every role, together: the box By model and By role open with, built as By
//! CLI's is. The period's weeks or days are columns stacked by part, beside three bars of what
//! the parts used: tokens, spend and agent time, each over its legend.

use argonauts_core::Spend;
use ratatui::style::Style;
use ratatui::text::Span;

use super::cli::{Stack, Stats, header_row, took_whole, whole_share};
use super::data::DayPart;
use super::draw::{self, Entry};
use crate::format;
use crate::kit::{Drawn, Frame, Kit};
use crate::theme::Role;

/// Cells between two entries of a legend.
const LEGEND_GAP: usize = 2;

/// A model or a role in the box.
pub(super) struct Part<'a> {
    pub name: String,
    pub style: Style,
    pub spent: Spend,
    /// The time its sessions took, together.
    pub took_secs: u64,
    /// Which of the period's parts are its own, for the chart.
    pub holds: Box<dyn Fn(&DayPart) -> bool + 'a>,
}

/// One bar: what it measures, each part's amount as its legend writes it, and the total.
struct Bar {
    what: &'static str,
    amounts: Vec<(u64, String)>,
    total: String,
}

impl Stats<'_> {
    /// `◆ all models  every day so far` and the period's totals, then the chart beside a bar
    /// each for the tokens, the spend and the agent time by `by`, a blank row between them.
    pub fn together(
        &self,
        kit: &Kit<'_>,
        frame: Frame,
        title: &str,
        by: &str,
        parts: &[Part<'_>],
    ) -> Drawn {
        let inner = frame.inner();
        let mut drawn = Drawn::new();
        let left = vec![
            kit.span(kit.glyphs.the_mod, Role::Question),
            Span::raw(" "),
            kit.strong(title.to_owned()),
            kit.dim(format!("  {}", self.period_words())),
        ];
        drawn.push(header_row(kit, left, &self.totals(), inner));
        drawn.push(Vec::new());

        let width = self.beside_chart(inner);
        let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
        for (k, bar) in self.bars(parts).into_iter().enumerate() {
            if k > 0 {
                rows.push(Vec::new());
            }
            rows.push(vec![
                kit.dim(format!("{} by {by} · {}", bar.what, bar.total)),
            ]);
            let whole: u64 = bar.amounts.iter().map(|(value, _)| value).sum();
            let stacked: Vec<(u64, Style)> = bar
                .amounts
                .iter()
                .zip(parts)
                .map(|((value, _), part)| (*value, part.style))
                .collect();
            rows.push(draw::stacked(kit, &stacked, width));
            let entries: Vec<Entry> = bar
                .amounts
                .into_iter()
                .zip(parts)
                .map(|((value, amount), part)| Entry {
                    style: part.style,
                    name: part.name.clone(),
                    amount,
                    note: whole_share(value, whole),
                })
                .collect();
            rows.extend(draw::legend_even(kit, &entries, width, LEGEND_GAP));
        }

        let stacks: Vec<Stack<'_>> = parts
            .iter()
            .map(|part| Stack {
                style: part.style,
                holds: part.holds.as_ref(),
            })
            .collect();
        for row in self.with_chart(kit, inner, rows, &stacks) {
            drawn.push(row);
        }
        frame.draw(kit, drawn)
    }

    /// The tokens, the spend and the agent time of `parts`, a bar each.
    fn bars(&self, parts: &[Part<'_>]) -> [Bar; 3] {
        let total = parts
            .iter()
            .fold(Spend::default(), |sum, part| sum.plus(&part.spent));
        let secs: u64 = parts.iter().map(|part| part.took_secs).sum();
        // Spend is shared out in hundredths of a cent, so a part under a cent keeps its share.
        let fine = |spent: &Spend| (spent.cost_usd * 10_000.0).round() as u64;
        [
            Bar {
                what: "tokens",
                amounts: parts
                    .iter()
                    .map(|part| {
                        let tokens = part.spent.total_tokens();
                        (tokens, self.tokens(tokens))
                    })
                    .collect(),
                total: self.tokens(total.total_tokens()),
            },
            Bar {
                what: "spend",
                amounts: parts
                    .iter()
                    .map(|part| (fine(&part.spent), format::spend(&part.spent)))
                    .collect(),
                total: format::spend(&total),
            },
            Bar {
                what: "agent time",
                amounts: parts
                    .iter()
                    .map(|part| (part.took_secs, took_whole(part.took_secs)))
                    .collect(),
                total: took_whole(secs),
            },
        ]
    }
}
