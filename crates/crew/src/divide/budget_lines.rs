//! Budget lines from `budget.lines` shares of the run budget, which must add up to the total.
//! With `budget.from_history`, a card's budget is the median spend of past cards of the same
//! task type once there are at least `budget.history_min`; otherwise `per_card_*`.

use argonauts_core::events::card::{CardBudget, CardSpec};
use argonauts_core::events::plan::{BudgetLine, BudgetPlan};
use argonauts_core::settings::budget::{
    BudgetAmount, BudgetLineKind, BudgetLines, BudgetSettings, BudgetUnit,
};

/// How far the shares may stray from 1 before the lines don't add up.
const SHARE_TOLERANCE: f64 = 1e-6;

pub fn plan(settings: &BudgetSettings) -> Result<BudgetPlan, String> {
    let total = run_total(settings).ok_or_else(|| {
        "No run budget is set. Set one of budget.run_usd, run_tokens, run_quota, or run_minutes."
            .to_owned()
    })?;
    let shares = shares(&settings.lines);
    if let Some((line, share)) = shares
        .iter()
        .find(|(_, share)| !share.is_finite() || *share < 0.0)
    {
        return Err(format!(
            "The {} budget line has a share of {share}.",
            line_name(*line)
        ));
    }
    let sum: f64 = shares.iter().map(|(_, share)| share).sum();
    if (sum - 1.0).abs() > SHARE_TOLERANCE {
        return Err(format!(
            "The budget lines add up to {sum:.3} of the run budget, not 1."
        ));
    }
    let mut lines: Vec<BudgetLine> = shares
        .iter()
        .map(|(line, share)| BudgetLine {
            line: *line,
            amount: BudgetAmount {
                unit: total.unit,
                amount: total.amount * share,
            },
        })
        .collect();
    // The reserve takes the rounding, so the lines add up to the total exactly.
    let others: f64 = lines[..lines.len() - 1]
        .iter()
        .map(|line| line.amount.amount)
        .sum();
    if let Some(reserve) = lines.last_mut() {
        reserve.amount.amount = (total.amount - others).max(0.0);
    }
    let share = |kind: BudgetLineKind| {
        shares
            .iter()
            .find(|(line, _)| *line == kind)
            .map_or(0.0, |(_, share)| *share)
    };
    // Spikes may not run and the reserve is only for trouble, so the range spans them.
    let reserve = share(BudgetLineKind::Reserve);
    Ok(BudgetPlan {
        lines,
        total,
        estimate_low: total.amount * (1.0 - reserve - share(BudgetLineKind::Spikes)),
        estimate_high: total.amount * (1.0 - reserve),
    })
}

/// Why the lines don't add up to the total, if they don't. For lines you edited.
pub fn check(plan: &BudgetPlan) -> Result<(), String> {
    if let Some(line) = plan
        .lines
        .iter()
        .find(|line| line.amount.unit != plan.total.unit)
    {
        return Err(format!(
            "The {} budget line is in another unit than the run budget.",
            line_name(line.line)
        ));
    }
    if let Some(line) = plan
        .lines
        .iter()
        .find(|line| !line.amount.amount.is_finite() || line.amount.amount < 0.0)
    {
        return Err(format!(
            "The {} budget line can't be {}.",
            line_name(line.line),
            line.amount.amount
        ));
    }
    let sum: f64 = plan.lines.iter().map(|line| line.amount.amount).sum();
    let total = plan.total.amount;
    if (sum - total).abs() > SHARE_TOLERANCE * total.max(1.0) {
        return Err(format!(
            "The budget lines add up to {}, not the run budget of {}.",
            amount_text(sum, plan.total.unit),
            amount_text(total, plan.total.unit)
        ));
    }
    Ok(())
}

/// What the plan should cost in dollars: every card's limit, plus the review and integration
/// lines, which grow with the cards. `None` when a card or one of those lines isn't in dollars.
pub fn estimate_usd(cards: &[CardSpec], budget: &BudgetPlan) -> Option<f64> {
    let dollars = |amount: &BudgetAmount| (amount.unit == BudgetUnit::Usd).then_some(amount.amount);
    let mut total = 0.0;
    for card in cards {
        total += dollars(card.budget.limit.as_ref()?)?;
    }
    for line in budget.lines.iter().filter(|line| {
        matches!(
            line.line,
            BudgetLineKind::Review | BudgetLineKind::Integration
        )
    }) {
        total += dollars(&line.amount)?;
    }
    Some(total)
}

/// `history` is past cards' spend in USD for this task type. History only sets dollar
/// budgets, so a per-card budget in another unit stays as it is.
pub fn card_budget(history: &[f64], settings: &BudgetSettings) -> CardBudget {
    let per_card = per_card(settings);
    let spent: Vec<f64> = history
        .iter()
        .copied()
        .filter(|usd| usd.is_finite() && *usd >= 0.0)
        .collect();
    let enough = !spent.is_empty() && spent.len() >= settings.history_min as usize;
    let dollars = per_card.is_none_or(|amount| amount.unit == BudgetUnit::Usd);
    let limit = if settings.from_history && enough && dollars {
        median(&spent).map(|amount| BudgetAmount {
            unit: BudgetUnit::Usd,
            amount,
        })
    } else {
        per_card
    };
    CardBudget {
        limit,
        ..CardBudget::default()
    }
}

pub fn median(values: &[f64]) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(sorted[n / 2]),
        _ => Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2.0),
    }
}

/// The key `budget <line> <share>` names, as in `budget.lines`.
pub fn line_name(line: BudgetLineKind) -> &'static str {
    match line {
        BudgetLineKind::GoalAndPlan => "goal_and_plan",
        BudgetLineKind::Spikes => "spikes",
        BudgetLineKind::Contracts => "contracts",
        BudgetLineKind::Cards => "cards",
        BudgetLineKind::Review => "review",
        BudgetLineKind::Integration => "integration",
        BudgetLineKind::Reserve => "reserve",
    }
}

pub fn line_named(name: &str) -> Option<BudgetLineKind> {
    ALL_LINES
        .into_iter()
        .find(|line| line_name(*line).eq_ignore_ascii_case(name))
}

pub fn amount_text(amount: f64, unit: BudgetUnit) -> String {
    match unit {
        BudgetUnit::Usd => format!("${amount:.2}"),
        BudgetUnit::Tokens => format!("{amount:.0} tokens"),
        BudgetUnit::Quota => format!("{:.0}% of quota", amount * 100.0),
        BudgetUnit::Minutes => format!("{amount:.0} minutes"),
    }
}

/// The reserve last, since it takes the rounding.
const ALL_LINES: [BudgetLineKind; 7] = [
    BudgetLineKind::GoalAndPlan,
    BudgetLineKind::Spikes,
    BudgetLineKind::Contracts,
    BudgetLineKind::Cards,
    BudgetLineKind::Review,
    BudgetLineKind::Integration,
    BudgetLineKind::Reserve,
];

fn shares(lines: &BudgetLines) -> Vec<(BudgetLineKind, f64)> {
    ALL_LINES
        .into_iter()
        .map(|line| {
            let share = match line {
                BudgetLineKind::GoalAndPlan => lines.goal_and_plan,
                BudgetLineKind::Spikes => lines.spikes,
                BudgetLineKind::Contracts => lines.contracts,
                BudgetLineKind::Cards => lines.cards,
                BudgetLineKind::Review => lines.review,
                BudgetLineKind::Integration => lines.integration,
                BudgetLineKind::Reserve => lines.reserve,
            };
            (line, share)
        })
        .collect()
}

fn run_total(settings: &BudgetSettings) -> Option<BudgetAmount> {
    amount_of(
        settings.run_usd,
        settings.run_tokens,
        settings.run_quota,
        settings.run_minutes,
    )
}

fn per_card(settings: &BudgetSettings) -> Option<BudgetAmount> {
    amount_of(
        settings.per_card_usd,
        settings.per_card_tokens,
        settings.per_card_quota,
        settings.per_card_minutes,
    )
}

/// The keys are one choice, so the first one set is the only one.
fn amount_of(
    usd: Option<f64>,
    tokens: Option<u64>,
    quota: Option<f64>,
    minutes: Option<f64>,
) -> Option<BudgetAmount> {
    let (unit, amount) = match (usd, tokens, quota, minutes) {
        (Some(usd), ..) => (BudgetUnit::Usd, usd),
        (_, Some(tokens), ..) => (BudgetUnit::Tokens, tokens as f64),
        (_, _, Some(quota), _) => (BudgetUnit::Quota, quota),
        (_, _, _, Some(minutes)) => (BudgetUnit::Minutes, minutes),
        _ => return None,
    };
    Some(BudgetAmount { unit, amount })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divide_budget_lines_add_up() {
        let settings = BudgetSettings {
            run_usd: Some(20.0),
            ..BudgetSettings::default()
        };
        let lines = plan(&settings).unwrap();
        assert_eq!(lines.lines.len(), 7);
        let sum: f64 = lines.lines.iter().map(|line| line.amount.amount).sum();
        assert!((sum - 20.0).abs() < 1e-9, "{sum}");
        assert!(check(&lines).is_ok());
        let cards = lines
            .lines
            .iter()
            .find(|line| line.line == BudgetLineKind::Cards)
            .unwrap();
        assert!((cards.amount.amount - 8.0).abs() < 1e-9);
        assert_eq!(lines.total.unit, BudgetUnit::Usd);
        assert!((lines.estimate_low - 16.0).abs() < 1e-9);
        assert!((lines.estimate_high - 18.0).abs() < 1e-9);

        // Awkward shares still add up exactly.
        let mut odd = settings.clone();
        odd.run_usd = Some(10.0);
        odd.lines = BudgetLines {
            goal_and_plan: 0.1,
            spikes: 0.1,
            contracts: 0.1,
            cards: 0.4,
            review: 0.1,
            integration: 0.1,
            reserve: 0.1,
        };
        let odd = plan(&odd).unwrap();
        let sum: f64 = odd.lines.iter().map(|line| line.amount.amount).sum();
        assert!(check(&odd).is_ok(), "{sum}");

        let mut over = settings.clone();
        over.lines.cards = 0.6;
        assert_eq!(
            plan(&over).unwrap_err(),
            "The budget lines add up to 1.200 of the run budget, not 1."
        );
        let mut tokens = settings.clone();
        tokens.run_usd = None;
        tokens.run_tokens = Some(200_000);
        assert_eq!(plan(&tokens).unwrap().total.unit, BudgetUnit::Tokens);
        let mut none = settings;
        none.run_usd = None;
        assert!(plan(&none).is_err());

        let mut edited = odd.clone();
        edited.lines[3].amount.amount += 1.0;
        assert_eq!(
            check(&edited).unwrap_err(),
            "The budget lines add up to $11.00, not the run budget of $10.00."
        );
    }

    #[test]
    fn card_budget_from_history_median() {
        let settings = BudgetSettings::default();
        assert_eq!(settings.history_min, 5);
        let five = [1.0, 2.5, 10.0, 2.0, 3.0];
        assert_eq!(
            card_budget(&five, &settings).limit,
            Some(BudgetAmount {
                unit: BudgetUnit::Usd,
                amount: 2.5
            })
        );
        let six = [1.0, 2.0, 3.0, 4.0, 100.0, 5.0];
        assert_eq!(card_budget(&six, &settings).limit.unwrap().amount, 3.5);
        // Too little history: the per-card budget.
        assert_eq!(
            card_budget(&five[..4], &settings).limit,
            Some(BudgetAmount {
                unit: BudgetUnit::Usd,
                amount: 3.0
            })
        );
        let off = BudgetSettings {
            from_history: false,
            ..BudgetSettings::default()
        };
        assert_eq!(card_budget(&five, &off).limit.unwrap().amount, 3.0);
        let minutes = BudgetSettings {
            per_card_usd: None,
            per_card_minutes: Some(20.0),
            ..BudgetSettings::default()
        };
        assert_eq!(
            card_budget(&five, &minutes).limit,
            Some(BudgetAmount {
                unit: BudgetUnit::Minutes,
                amount: 20.0
            })
        );
    }

    #[test]
    fn the_estimate_grows_with_the_cards() {
        let settings = BudgetSettings {
            run_usd: Some(20.0),
            per_card_usd: Some(3.0),
            ..BudgetSettings::default()
        };
        let budget = plan(&settings).unwrap();
        let line = |kind| {
            budget
                .lines
                .iter()
                .find(|line| line.line == kind)
                .map_or(0.0, |line| line.amount.amount)
        };
        let after_cards = line(BudgetLineKind::Review) + line(BudgetLineKind::Integration);
        let cards = |n: usize| {
            let keys: Vec<serde_json::Value> = (0..n)
                .map(|i| {
                    serde_json::json!({
                        "key": format!("c{i}"), "title": "t", "task": "t", "kind": "build",
                        "owned_paths": [format!("src/c{i}/**")], "read_only_paths": [],
                        "depends_on": [], "criteria": ["G1"]
                    })
                })
                .collect();
            let proposal = serde_json::from_value(serde_json::json!({ "cards": keys })).unwrap();
            let (mut cards, _) = super::super::graph::convert(&proposal);
            for card in &mut cards {
                card.budget = card_budget(&[], &settings);
            }
            cards
        };
        let two = estimate_usd(&cards(2), &budget).unwrap();
        let twelve = estimate_usd(&cards(12), &budget).unwrap();
        assert!((two - (6.0 + after_cards)).abs() < 1e-9, "{two}");
        assert!((twelve - (36.0 + after_cards)).abs() < 1e-9, "{twelve}");

        let mut minutes = cards(1);
        minutes[0].budget.limit = Some(BudgetAmount {
            unit: BudgetUnit::Minutes,
            amount: 30.0,
        });
        assert_eq!(estimate_usd(&minutes, &budget), None);
    }
}
