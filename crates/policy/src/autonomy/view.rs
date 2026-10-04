//! What `/approvals` and `argonauts approvals` show: each point's mode and decider, its shadow
//! record, and the spot checks waiting.

use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::policy::{ApprovalRow, ApprovalsView};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::ApprovalMode;
use time::OffsetDateTime;

use super::records::{self, Record};
use super::scoring::{self, Scope};

/// The decision points with approval settings, in the order `/approvals` lists them.
pub const KINDS: [CheckpointKind; 7] = [
    CheckpointKind::Goal,
    CheckpointKind::Plan,
    CheckpointKind::Merge,
    CheckpointKind::RecipePromotion,
    CheckpointKind::Escalation,
    CheckpointKind::Budget,
    CheckpointKind::ChangeRequest,
];

/// A row counts every recipe of its kind. `supervised_everywhere` is set by `auto-off`, or when
/// the records can't be trusted.
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
                Scope::AllRecipes,
                Some(&point.fingerprint),
                now,
            );
            // The full auto switch covers the checkpoints a run opens, not a promotion.
            let own = match kind {
                CheckpointKind::RecipePromotion => point.mode,
                _ => approvals.run_mode(point.mode),
            };
            let mode = if supervised_everywhere {
                ApprovalMode::Supervised
            } else if own.goes_ahead() && tally.demoted {
                ApprovalMode::Shadow
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
        approver: approvals.approver.model.clone(),
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn every_kind_with_settings_gets_a_row() {
        let mut settings = Settings::default();
        settings.approvals.merge.mode = ApprovalMode::Auto;
        settings.approvals.approver.model = Some("claude/haiku".parse().expect("worker"));
        let now = datetime!(2026-09-29 10:00 UTC);

        let view = build(&[], &settings, now, false);
        let kinds: Vec<_> = view.points.iter().map(|row| row.kind).collect();
        assert_eq!(kinds, KINDS);
        assert_eq!(view.points[2].mode, ApprovalMode::Auto);
        assert_eq!(view.spot_checks_waiting, 0);
        assert_eq!(view.approver, settings.approvals.approver.model);

        let off = build(&[], &settings, now, true);
        assert!(
            off.points
                .iter()
                .all(|row| row.mode == ApprovalMode::Supervised)
        );
    }

    /// The full auto switch shows on every checkpoint a run opens, and not on a promotion.
    #[test]
    fn the_full_auto_switch_shows_on_each_run_checkpoint() {
        let mut settings = Settings::default();
        settings.approvals.full_auto = true;
        let now = datetime!(2026-09-29 10:00 UTC);
        let view = build(&[], &settings, now, false);
        for row in &view.points {
            let expected = match row.kind {
                CheckpointKind::RecipePromotion => ApprovalMode::Auto,
                _ => ApprovalMode::FullAuto,
            };
            assert_eq!(row.mode, expected, "{:?}", row.kind);
        }
    }
}
