//! Limit lines from `limits.lines` shares of the run limit, which must add up to the total.
//! With `limits.from_history`, a task's limit is the median spend of past tasks of the same
//! task type once there are at least `limits.history_min`; otherwise `per_task_*`.

use argonauts_core::events::plan::{LimitLine, LimitPlan};
use argonauts_core::events::task::{TaskLimit, TaskSpec};
use argonauts_core::settings::limit::{
    LimitAmount, LimitLineKind, LimitLines, LimitUnit, SpendLimits,
};

/// How far the shares may stray from 1 before the lines don't add up.
const SHARE_TOLERANCE: f64 = 1e-6;

/// The run's limit split into its lines, or `None` when no run limit is set: then nothing
/// is split and no line can run out.
pub fn plan(settings: &SpendLimits) -> Result<Option<LimitPlan>, String> {
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
    let mut lines: Vec<LimitLine> = shares
        .iter()
        .map(|(line, share)| LimitLine {
            line: *line,
            amount: LimitAmount {
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
    let share = |kind: LimitLineKind| {
        shares
            .iter()
            .find(|(line, _)| *line == kind)
            .map_or(0.0, |(_, share)| *share)
    };
    // Experiments may not run and the reserve is only for trouble, so the range spans them.
    let reserve = share(LimitLineKind::Reserve);
    Ok(Some(LimitPlan {
        lines,
        total,
        estimate_low: total.amount * (1.0 - reserve - share(LimitLineKind::Experiments)),
        estimate_high: total.amount * (1.0 - reserve),
    }))
}

/// Why the lines don't add up to the total, if they don't. For lines you edited.
pub fn check(plan: &LimitPlan) -> Result<(), String> {
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

/// What the plan should cost in dollars: every task's limit, plus the review and merge
/// lines, which grow with the tasks. `None` when a task or one of those lines isn't in dollars.
pub fn estimate_usd(tasks: &[TaskSpec], limit: &LimitPlan) -> Option<f64> {
    let dollars = |amount: &LimitAmount| (amount.unit == LimitUnit::Usd).then_some(amount.amount);
    let mut total = 0.0;
    for task in tasks {
        total += dollars(task.limit.limit.as_ref()?)?;
    }
    for line in limit
        .lines
        .iter()
        .filter(|line| matches!(line.line, LimitLineKind::Review | LimitLineKind::Merge))
    {
        total += dollars(&line.amount)?;
    }
    Some(total)
}

/// `history` is past tasks' spend in USD for this task type. History only sets dollar
/// limits, so a per-task limit in another unit stays as it is. With no limit set for the
/// run or its tasks, a task has none: history alone never limits one.
pub fn task_limit(history: &[f64], settings: &SpendLimits) -> TaskLimit {
    let per_task = settings.task_limit();
    let spent: Vec<f64> = history
        .iter()
        .copied()
        .filter(|usd| usd.is_finite() && *usd >= 0.0)
        .collect();
    let enough = !spent.is_empty() && spent.len() >= settings.history_min as usize;
    let dollars = per_task.is_none_or(|amount| amount.unit == LimitUnit::Usd);
    let limited = per_task.is_some() || settings.run_limit().is_some();
    let limit = if settings.from_history && enough && dollars && limited {
        median(&spent).map(|amount| LimitAmount {
            unit: LimitUnit::Usd,
            amount,
        })
    } else {
        per_task
    };
    TaskLimit {
        limit,
        ..TaskLimit::default()
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

/// The key `limit <line> <share>` names, as in `limits.lines`.
pub fn line_name(line: LimitLineKind) -> &'static str {
    match line {
        LimitLineKind::GoalAndPlan => "goal_and_plan",
        LimitLineKind::Experiments => "experiments",
        LimitLineKind::Contracts => "contracts",
        LimitLineKind::Tasks => "tasks",
        LimitLineKind::Review => "review",
        LimitLineKind::Merge => "merge",
        LimitLineKind::Reserve => "reserve",
    }
}

pub fn line_named(name: &str) -> Option<LimitLineKind> {
    ALL_LINES
        .into_iter()
        .find(|line| line_name(*line).eq_ignore_ascii_case(name))
}

pub fn amount_text(amount: f64, unit: LimitUnit) -> String {
    match unit {
        LimitUnit::Usd => format!("${amount:.2}"),
        LimitUnit::Tokens => format!("{amount:.0} tokens"),
        LimitUnit::PlanUsage => format!("{:.0}% of plan usage", amount * 100.0),
        LimitUnit::Minutes => format!("{amount:.0} minutes"),
    }
}

/// The reserve last, since it takes the rounding.
const ALL_LINES: [LimitLineKind; 7] = [
    LimitLineKind::GoalAndPlan,
    LimitLineKind::Experiments,
    LimitLineKind::Contracts,
    LimitLineKind::Tasks,
    LimitLineKind::Review,
    LimitLineKind::Merge,
    LimitLineKind::Reserve,
];

fn shares(lines: &LimitLines) -> Vec<(LimitLineKind, f64)> {
    ALL_LINES
        .into_iter()
        .map(|line| {
            let share = match line {
                LimitLineKind::GoalAndPlan => lines.goal_and_plan,
                LimitLineKind::Experiments => lines.experiments,
                LimitLineKind::Contracts => lines.contracts,
                LimitLineKind::Tasks => lines.tasks,
                LimitLineKind::Review => lines.review,
                LimitLineKind::Merge => lines.merge,
                LimitLineKind::Reserve => lines.reserve,
            };
            (line, share)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divide_limit_lines_add_up() {
        let settings = SpendLimits {
            spend_per_run: Some(20.0),
            ..SpendLimits::default()
        };
        let lines = plan(&settings).unwrap().unwrap();
        assert_eq!(lines.lines.len(), 7);
        let sum: f64 = lines.lines.iter().map(|line| line.amount.amount).sum();
        assert!((sum - 20.0).abs() < 1e-9, "{sum}");
        assert!(check(&lines).is_ok());
        let tasks = lines
            .lines
            .iter()
            .find(|line| line.line == LimitLineKind::Tasks)
            .unwrap();
        assert!((tasks.amount.amount - 8.0).abs() < 1e-9);
        assert_eq!(lines.total.unit, LimitUnit::Usd);
        assert!((lines.estimate_low - 16.0).abs() < 1e-9);
        assert!((lines.estimate_high - 18.0).abs() < 1e-9);

        // Awkward shares still add up exactly.
        let mut odd = settings.clone();
        odd.spend_per_run = Some(10.0);
        odd.lines = LimitLines {
            goal_and_plan: 0.1,
            experiments: 0.1,
            contracts: 0.1,
            tasks: 0.4,
            review: 0.1,
            merge: 0.1,
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
            LimitUnit::Tokens
        );
        // No run limit is nothing to split, whatever the shares say.
        over.spend_per_run = None;
        assert_eq!(plan(&over), Ok(None));
        assert_eq!(plan(&SpendLimits::default()), Ok(None));

        let mut edited = odd.clone();
        edited.lines[3].amount.amount += 1.0;
        assert_eq!(
            check(&edited).unwrap_err(),
            "The limit lines add up to $11.00, not the run limit of $10.00."
        );
    }

    #[test]
    fn task_limit_from_history_median() {
        let five = [1.0, 2.5, 10.0, 2.0, 3.0];
        // With no limit set, a task has none, however much history there is.
        assert_eq!(task_limit(&five, &SpendLimits::default()).limit, None);
        let run_only = SpendLimits {
            spend_per_run: Some(20.0),
            ..SpendLimits::default()
        };
        assert_eq!(task_limit(&five, &run_only).limit.unwrap().amount, 2.5);
        assert_eq!(task_limit(&five[..4], &run_only).limit, None);

        let settings = SpendLimits {
            per_task_usd: Some(3.0),
            ..SpendLimits::default()
        };
        assert_eq!(settings.history_min, 5);
        assert_eq!(
            task_limit(&five, &settings).limit,
            Some(LimitAmount {
                unit: LimitUnit::Usd,
                amount: 2.5
            })
        );
        let six = [1.0, 2.0, 3.0, 4.0, 100.0, 5.0];
        assert_eq!(task_limit(&six, &settings).limit.unwrap().amount, 3.5);
        // Too little history: the per-task limit.
        assert_eq!(
            task_limit(&five[..4], &settings).limit,
            Some(LimitAmount {
                unit: LimitUnit::Usd,
                amount: 3.0
            })
        );
        let off = SpendLimits {
            from_history: false,
            ..settings.clone()
        };
        assert_eq!(task_limit(&five, &off).limit.unwrap().amount, 3.0);
        let minutes = SpendLimits {
            per_task_usd: None,
            per_task_minutes: Some(20.0),
            ..SpendLimits::default()
        };
        assert_eq!(
            task_limit(&five, &minutes).limit,
            Some(LimitAmount {
                unit: LimitUnit::Minutes,
                amount: 20.0
            })
        );
    }

    #[test]
    fn the_estimate_grows_with_the_tasks() {
        let settings = SpendLimits {
            spend_per_run: Some(20.0),
            per_task_usd: Some(3.0),
            ..SpendLimits::default()
        };
        let limit = plan(&settings).unwrap().unwrap();
        let line = |kind| {
            limit
                .lines
                .iter()
                .find(|line| line.line == kind)
                .map_or(0.0, |line| line.amount.amount)
        };
        let after_tasks = line(LimitLineKind::Review) + line(LimitLineKind::Merge);
        let tasks = |n: usize| {
            let keys: Vec<serde_json::Value> = (0..n)
                .map(|i| {
                    serde_json::json!({
                        "key": format!("c{i}"), "title": "t", "task": "t", "kind": "build",
                        "owned_paths": [format!("src/c{i}/**")], "read_only_paths": [],
                        "depends_on": [], "criteria": ["G1"]
                    })
                })
                .collect();
            let proposal = serde_json::from_value(serde_json::json!({ "tasks": keys })).unwrap();
            let (mut tasks, _) = super::super::graph::convert(&proposal);
            for task in &mut tasks {
                task.limit = task_limit(&[], &settings);
            }
            tasks
        };
        let two = estimate_usd(&tasks(2), &limit).unwrap();
        let twelve = estimate_usd(&tasks(12), &limit).unwrap();
        assert!((two - (6.0 + after_tasks)).abs() < 1e-9, "{two}");
        assert!((twelve - (36.0 + after_tasks)).abs() < 1e-9, "{twelve}");

        let mut minutes = tasks(1);
        minutes[0].limit.limit = Some(LimitAmount {
            unit: LimitUnit::Minutes,
            amount: 30.0,
        });
        assert_eq!(estimate_usd(&minutes, &limit), None);
    }
}
