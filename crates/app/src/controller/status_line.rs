//! The header and status line's data.

use argonauts_core::RunId;
use argonauts_core::Spend;
use argonauts_core::app::{QuotaChip, StatusLine};
use argonauts_core::limits::{HealthBoard, QuotaBoard};
use argonauts_core::names::{VendorId, WorkerRef};
use argonauts_core::settings::approvals::{ApprovalMode, ApprovalSettings};
use argonauts_core::settings::budget::BudgetAmount;

use super::Ctx;
use super::turn::chat_agent;

/// What the attached run adds to the status line.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RunStatusParts {
    pub run: Option<RunId>,
    pub spent: Option<Spend>,
    pub limit: Option<BudgetAmount>,
    /// The attached run's mode, else Shift+Tab's for this app session. `None` leaves it to
    /// the settings, which every new run starts from.
    pub approval_mode: Option<ApprovalMode>,
}

pub(crate) fn status_line(ctx: &Ctx, title: &str, run: RunStatusParts) -> StatusLine {
    let settings = ctx.settings();
    StatusLine {
        title: title.to_owned(),
        repo: ctx.env.display_repo(),
        preset: settings.preset.clone(),
        run: run.run,
        default_agent: header_agent(ctx).0,
        quotas: quota_chips(
            &settings.crew,
            ctx.services.quota.as_ref(),
            ctx.services.health.as_ref(),
        ),
        spent: run.spent,
        limit: run.limit,
        narrator: settings.ui.narrator,
        approval_mode: run
            .approval_mode
            .unwrap_or_else(|| settings_mode(&settings.approvals)),
    }
}

/// The chat's agent as the header names it. The status line carries the same, so it names an
/// agent only once you change it. With no model set the CLI runs its own default: the one its
/// model list marks, else the one the catalog's "Default (recommended)" runs. With no effort
/// set it runs the level that model starts at, as the CLI says, else the catalog.
pub(crate) fn header_agent(ctx: &Ctx) -> (WorkerRef, Option<String>) {
    let settings = ctx.settings();
    let (mut agent, effort) = chat_agent(settings, &settings.default_agent);
    let registry = &ctx.services.registry;
    let catalog = registry.catalog();
    if agent.model.is_none() {
        let marked = registry
            .list()
            .into_iter()
            .find(|entry| entry.cli_default && entry.id.worker == agent.worker)
            .and_then(|entry| entry.id.model);
        let harness = WorkerRef::new(agent.worker.clone(), None);
        agent.model = marked.or_else(|| catalog.entry(&harness)?.runs.clone());
    }
    let effort = effort
        .or_else(|| registry.get(&agent)?.default_effort)
        .or_else(|| catalog.entry(&agent)?.default_effort.clone());
    (agent, effort)
}

/// One mode for the settings: the checkpoints' own when they agree, else the least autonomous
/// of them, since that one still waits for you.
fn settings_mode(approvals: &ApprovalSettings) -> ApprovalMode {
    [
        approvals.goal.mode,
        approvals.plan.mode,
        approvals.merge.mode,
        approvals.recipe_promotion.mode,
        approvals.escalation.mode,
        approvals.budget.mode,
        approvals.change_request.mode,
    ]
    .into_iter()
    .min_by_key(|mode| match mode {
        ApprovalMode::Supervised => 0,
        ApprovalMode::Shadow => 1,
        ApprovalMode::Auto => 2,
    })
    .unwrap_or_default()
}

/// One chip per vendor, in `crew` order, so two models of one vendor share a chip.
pub(crate) fn quota_chips(
    crew: &[WorkerRef],
    quota: &dyn QuotaBoard,
    health: &dyn HealthBoard,
) -> Vec<QuotaChip> {
    let mut seen: Vec<VendorId> = Vec::new();
    let mut chips = Vec::new();
    for worker in crew {
        let vendor = worker.vendor();
        if seen.contains(&vendor) {
            continue;
        }
        chips.push(QuotaChip {
            label: vendor_label(&vendor),
            reading: quota.get(&vendor),
            health: health.state(&vendor).state,
        });
        seen.push(vendor);
    }
    chips
}

/// The short name the status line shows, such as `Claude` for `claude-code`.
pub(crate) fn vendor_label(vendor: &VendorId) -> String {
    match vendor.0.as_str() {
        "claude-code" => "Claude".to_owned(),
        "codex" => "Codex".to_owned(),
        "antigravity-cli" => "agy".to_owned(),
        "opencode" => "OpenCode".to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::{HealthState, QuotaReading, QuotaSource, QuotaStatus};
    use argonauts_testkit::TempRepo;
    use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};
    use time::OffsetDateTime;

    use crate::controller::test_support::{HarnessOptions, TestCtx};

    fn reading(vendor: &str, used: f64) -> QuotaReading {
        QuotaReading {
            vendor: VendorId(vendor.to_owned()),
            status: QuotaStatus::Allowed,
            used: Some(used),
            resets_at: None,
            window: Some("five_hour".to_owned()),
            source: QuotaSource::RateLimitEvent,
            observed_at: OffsetDateTime::UNIX_EPOCH,
            detail: None,
        }
    }

    /// The status line carries the agent the header names, so the mode line names none until
    /// you change it: the model your settings give, or the one the CLI runs by default.
    #[test]
    fn the_status_line_names_the_agent_the_header_does() {
        let (parts, _) = TestCtx::new(HarnessOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(
                    repo.global_config(),
                    "default_agent = \"codex\"\n\n[agents.codex]\nmodel = \"gpt-5.6-luna\"\n",
                )
                .unwrap();
            })),
            ..HarnessOptions::default()
        });
        let status = status_line(&parts.ctx, "", RunStatusParts::default());
        assert_eq!(status.default_agent.to_string(), "codex/gpt-5.6-luna");

        let (parts, _) = TestCtx::new(HarnessOptions::default());
        let status = status_line(&parts.ctx, "", RunStatusParts::default());
        assert!(
            status.default_agent.model.is_some(),
            "{}",
            status.default_agent
        );
        assert_eq!(status.default_agent, header_agent(&parts.ctx).0);
    }

    #[test]
    fn one_chip_per_vendor_in_crew_order() {
        let quota = MemoryQuotaBoard::default();
        let health = MemoryHealthBoard::default();
        quota.observe(reading("codex", 0.38));
        let crew: Vec<WorkerRef> = ["codex/gpt-5.6", "claude-code", "codex/gpt-5.6-luna"]
            .iter()
            .map(|worker| worker.parse().unwrap())
            .collect();
        health.set(
            &VendorId("claude-code".to_owned()),
            HealthState::Down,
            Some("overloaded"),
        );
        let chips = quota_chips(&crew, &quota, &health);
        let labels: Vec<&str> = chips.iter().map(|chip| chip.label.as_str()).collect();
        assert_eq!(labels, ["Codex", "Claude"]);
        assert_eq!(chips[0].reading.as_ref().and_then(|r| r.used), Some(0.38));
        assert_eq!(chips[1].reading, None);
        assert_eq!(chips[0].health, HealthState::Healthy);
        assert_eq!(chips[1].health, HealthState::Down);
    }
}
