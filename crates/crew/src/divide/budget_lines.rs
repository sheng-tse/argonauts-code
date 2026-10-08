//! Budget lines from `limits.lines` shares of the run budget, which must add up to the total.
//! With `limits.from_history`, a card's budget is the median spend of past cards of the same
//! task type once there are at least `limits.history_min`; otherwise `per_task_*`.

use argonauts_core::events::card::{CardBudget, CardSpec};
use argonauts_core::events::plan::{BudgetLine, BudgetPlan};
use argonauts_core::settings::budget::{
    BudgetAmount, BudgetLineKind, BudgetLines, BudgetSettings, BudgetUnit,
};

/// How far the shares may stray from 1 before the lines don't add up.
const SHARE_TOLERANCE: f64 = 1e-6;

/// The run's limit split into its lines, or `None` when no run limit is set: then nothing
/// is split and no line can run out.
pub fn plan(settings: &BudgetSettings) -> Result<Option<BudgetPlan>, String> {
    let Some(total) = settings.run_limit() else {
        return Ok(None);
    };
    let shares = shares(&settings.lines);
    if let Some((line, share)) = shares
        .iter()
        .find(|(_, share)| !share.is_finite() || *share < 0.0)
    {
        return Err(format!(
            "The {} limit line has a share of {share}.",
            line_name(*line)
        ));
    }
    let sum: f64 = shares.iter().map(|(_, share)| share).sum();
    if (sum - 1.0).abs() > SHARE_TOLERANCE {
        return Err(format!(
            "The limit lines add up to {sum:.3} of the run limit, not 1."
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
    Ok(Some(BudgetPlan {
        lines,
        total,
        estimate_low: total.amount * (1.0 - reserve - share(BudgetLineKind::Spikes)),
        estimate_high: total.amount * (1.0 - reserve),
    }))
}

/// Why the lines don't add up to the total, if they don't. For lines you edited.
pub fn check(plan: &BudgetPlan) -> Result<(), String> {
    if let Some(line) = plan
        .lines
        .iter()
        .find(|line| line.amount.unit != plan.total.unit)
    {
        return Err(format!(
            "The {} limit line is in another unit than the run limit.",
            line_name(line.line)
        ));
    }
    if let Some(line) = plan
        .lines
        .iter()
        .find(|line| !line.amount.amount.is_finite() || line.amount.amount < 0.0)
    {
        return Err(format!(
            "The {} limit line can't be {}.",
            line_name(line.line),
            line.amount.amount
        ));
    }
    let sum: f64 = plan.lines.iter().map(|line| line.amount.amount).sum();
    let total = plan.total.amount;
    if (sum - total).abs() > SHARE_TOLERANCE * total.max(1.0) {
        return Err(format!(
            "The limit lines add up to {}, not the run limit of {}.",
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
/// budgets, so a per-card budget in another unit stays as it is. With no limit set for the
/// run or its tasks, a task has none: history alone never limits one.
pub fn card_budget(history: &[f64], settings: &BudgetSettings) -> CardBudget {
    let per_task = settings.task_limit();
    let spent: Vec<f64> = history
        .iter()
        .copied()
        .filter(|usd| usd.is_finite() && *usd >= 0.0)
        .collect();
    let enough = !spent.is_empty() && spent.len() >= settings.history_min as usize;
    let dollars = per_task.is_none_or(|amount| amount.unit == BudgetUnit::Usd);
    let limited = per_task.is_some() || settings.run_limit().is_some();
    let limit = if settings.from_history && enough && dollars && limited {
        median(&spent).map(|amount| BudgetAmount {
            unit: BudgetUnit::Usd,
            amount,
        })
    } else {
        per_task
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

/// The key `budget <line> <share>` names, as in `limits.lines`.
pub fn line_name(line: BudgetLineKind) -> &'static str {
    match line {
        BudgetLineKind::GoalAndPlan => "goal_and_plan",
        BudgetLineKind::Spikes => "experiments",
        BudgetLineKind::Contracts => "contracts",
        BudgetLineKind::Cards => "tasks",
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
        BudgetUnit::Quota => format!("{:.0}% of plan usage", amount * 100.0),
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
                BudgetLineKind::Spikes => lines.experiments,
                BudgetLineKind::Contracts => lines.contracts,
                BudgetLineKind::Cards => lines.tasks,
                BudgetLineKind::Review => lines.review,
                BudgetLineKind::Integration => lines.integration,
                BudgetLineKind::Reserve => lines.reserve,
            };
            (line, share)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divide_budget_lines_add_up() {
        let settings = BudgetSettings {
            spend_per_run: Some(20.0),
            ..BudgetSettings::default()
        };
        let lines = plan(&settings).unwrap().unwrap();
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
        odd.spend_per_run = Some(10.0);
        odd.lines = BudgetLines {
            goal_and_plan: 0.1,
            experiments: 0.1,
            contracts: 0.1,
            tasks: 0.4,
            review: 0.1,
            integration: 0.1,
            reserve: 0.1,
        };
        let odd = plan(&odd).unwrap().unwrap();
        let sum: f64 = odd.lines.iter().map(|line| line.amount.amount).sum();
        assert!(check(&odd).is_ok(), "{sum}");

        let mut over = settings.clone();
        over.lines.tasks = 0.6;
        assert_eq!(
            plan(&over).unwrap_err(),
            "The limit lines add up to 1.200 of the run limit, not 1."
        );
        let mut tokens = settings.clone();
        tokens.spend_per_run = None;
        tokens.run_tokens = Some(200_000);
        assert_eq!(
            plan(&tokens).unwrap().unwrap().total.unit,
            BudgetUnit::Tokens
        );
        // No run limit is nothing to split, whatever the shares say.
        over.spend_per_run = None;
        assert_eq!(plan(&over), Ok(None));
        assert_eq!(plan(&BudgetSettings::default()), Ok(None));

        let mut edited = odd.clone();
        edited.lines[3].amount.amount += 1.0;
        assert_eq!(
            check(&edited).unwrap_err(),
            "The limit lines add up to $11.00, not the run limit of $10.00."
        );
    }

    #[test]
    fn card_budget_from_history_median() {
        let five = [1.0, 2.5, 10.0, 2.0, 3.0];
        // With no limit set, a task has none, however much history there is.
        assert_eq!(card_budget(&five, &BudgetSettings::default()).limit, None);
        let run_only = BudgetSettings {
            spend_per_run: Some(20.0),
            ..BudgetSettings::default()
        };
        assert_eq!(card_budget(&five, &run_only).limit.unwrap().amount, 2.5);
        assert_eq!(card_budget(&five[..4], &run_only).limit, None);

        let settings = BudgetSettings {
            per_task_usd: Some(3.0),
            ..BudgetSettings::default()
        };
        assert_eq!(settings.history_min, 5);
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
            ..settings.clone()
        };
        assert_eq!(card_budget(&five, &off).limit.unwrap().amount, 3.0);
        let minutes = BudgetSettings {
            per_task_usd: None,
            per_task_minutes: Some(20.0),
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
            spend_per_run: Some(20.0),
            per_task_usd: Some(3.0),
            ..BudgetSettings::default()
        };
        let budget = plan(&settings).unwrap().unwrap();
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
