//! What `/approvals` and `argonauts approvals` show: each point's mode and decider, its auto
//! preview record, and the spot checks waiting.

use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::policy::{ApprovalRow, ApprovalsView};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;
use time::OffsetDateTime;

use super::records::{self, Record};
use super::scoring::{self, Scope};

/// The decision points with approval settings, in the order `/approvals` lists them.
pub const KINDS: [ApprovalKind; 7] = [
    ApprovalKind::Understand,
    ApprovalKind::Plan,
    ApprovalKind::Merge,
    ApprovalKind::WorkflowPromotion,
    ApprovalKind::Stuck,
    ApprovalKind::Limit,
    ApprovalKind::ChangeRequest,
];

/// A row counts every learned workflow of its kind. `supervised_everywhere` is set by `auto-off`,
/// or when the records can't be trusted.
pub fn build(
    records: &[Record],
    settings: &Settings,
    now: OffsetDateTime,
    supervised_everywhere: bool,
) -> ApprovalsView {
    let approvals = &settings.approvals;
    let suggest_after = approvals.suggest_auto_after_matches.max(1);
    let points = KINDS
        .into_iter()
        .filter_map(|kind| {
            let point = scoring::point_settings(approvals, kind)?;
            let tally = scoring::tally(
                records,
                kind,
                Scope::AllLearnedWorkflows,
                Some(&point.fingerprint),
                now,
            );
            // The autopilot switch covers the approvals a run opens, not a promotion.
            let own = match kind {
                ApprovalKind::WorkflowPromotion => point.mode,
                _ => approvals.run_mode(point.mode),
            };
            let mode = if supervised_everywhere {
                ApprovalMode::Supervised
            } else if own.goes_ahead() && tally.demoted {
                ApprovalMode::AutoPreview
            } else {
                own
            };
            Some(ApprovalRow {
                kind,
                mode,
                decide_with: point.decide_with,
                matches_in_a_row: tally.matches_in_a_row,
                misses: tally.misses,
                suggest_auto: tally.matches_in_a_row >= suggest_after,
            })
        })
        .collect();
    ApprovalsView {
        points,
        spot_checks_waiting: records::spot_checks_waiting(records),
        // Left to the judge, the view names the one your crew set, so `/approvals` says who
        // checks instead of how the policy would pick one.
        judge: approvals
            .judge_model()
            .cloned()
            .or_else(|| crate::judge::judges(settings).into_iter().next()),
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use argonauts_core::names::ModelRef;

    use super::*;

    #[test]
    fn every_kind_with_settings_gets_a_row() {
        let mut settings = Settings::default();
        settings.approvals.merge.mode = ApprovalMode::Auto;
        let model: ModelRef = "claude/haiku".parse().expect("worker");
        settings.approvals.decides = model.clone().into();
        let now = datetime!(2026-09-29 10:00 UTC);

        let view = build(&[], &settings, now, false);
        let kinds: Vec<_> = view.points.iter().map(|row| row.kind).collect();
        assert_eq!(kinds, KINDS);
        assert_eq!(view.points[2].mode, ApprovalMode::Auto);
        assert_eq!(view.spot_checks_waiting, 0);
        assert_eq!(view.judge, Some(model));

        let off = build(&[], &settings, now, true);
        assert!(
            off.points
                .iter()
                .all(|row| row.mode == ApprovalMode::Supervised)
        );
    }

    /// With the decision left to the judge, the view names the judge your crew set, and
    /// nobody when the crew names none.
    #[test]
    fn the_judge_your_crew_names_is_who_checks() {
        let mut settings = Settings::default();
        let now = datetime!(2026-09-29 10:00 UTC);
        assert_eq!(build(&[], &settings, now, false).judge, None);
        let codex: ModelRef = "codex/gpt-5.6".parse().expect("worker");
        settings.crew.judge = vec![codex.clone()];
        assert_eq!(build(&[], &settings, now, false).judge, Some(codex));
        let picked: ModelRef = "claude/haiku".parse().expect("worker");
        settings.approvals.decides = picked.clone().into();
        assert_eq!(build(&[], &settings, now, false).judge, Some(picked));
    }

    /// The autopilot switch shows on every approval a run opens, and not on a promotion.
    #[test]
    fn the_autopilot_switch_shows_on_each_run_approval() {
        let mut settings = Settings::default();
        settings.approvals.autopilot = true;
        let now = datetime!(2026-09-29 10:00 UTC);
        let view = build(&[], &settings, now, false);
        for row in &view.points {
            let expected = match row.kind {
                ApprovalKind::WorkflowPromotion => ApprovalMode::Auto,
                _ => ApprovalMode::Autopilot,
            };
            assert_eq!(row.mode, expected, "{:?}", row.kind);
        }
    }
}
