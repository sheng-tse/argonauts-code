//! `argonauts approvals` and `argonauts auto off`.

use std::process::ExitCode;

use argonauts_app::ApprovalsReport;

use crate::text::{count, words};

pub async fn show() -> anyhow::Result<ExitCode> {
    let report = argonauts_app::approvals_report(None).await?;
    print!("{}", report_text(&report));
    Ok(ExitCode::SUCCESS)
}

fn report_text(report: &ApprovalsReport) -> String {
    let view = &report.view;
    let mut out = String::new();
    if view.points.is_empty() {
        out.push_str("No approval record yet.\n");
    }
    for point in &view.points {
        out.push_str(&format!(
            "{}: {}, decided by {}",
            words(&point.kind),
            words(&point.mode),
            words(&point.decide_with)
        ));
        if point.matches_in_a_row > 0 || point.misses > 0 {
            out.push_str(&format!(
                " · agreed with you {} in a row, {}",
                count(u64::from(point.matches_in_a_row), "time", "times"),
                count(u64::from(point.misses), "miss", "misses")
            ));
        }
        if point.suggest_auto {
            out.push_str(" · ready for auto");
        }
        out.push('\n');
    }
    if view.spot_checks_waiting > 0 {
        out.push_str(&format!(
            "{} waiting for you.\n",
            count(
                u64::from(view.spot_checks_waiting),
                "spot check",
                "spot checks"
            )
        ));
    }
    if let Some(approver) = &view.approver {
        out.push_str(&format!("The approver model is {approver}.\n"));
    }
    for note in &report.notes {
        out.push_str(note);
        out.push('\n');
    }
    out
}

pub fn auto_off() -> anyhow::Result<ExitCode> {
    argonauts_app::auto_off()?;
    println!("Auto is off. Every checkpoint in every run waits for you until you turn it back on.");
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::checkpoint::CheckpointKind;
    use argonauts_core::policy::{ApprovalRow, ApprovalsView};
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};

    use super::*;

    #[test]
    fn each_checkpoint_shows_its_mode_and_record() {
        let report = ApprovalsReport {
            view: ApprovalsView {
                points: vec![ApprovalRow {
                    kind: CheckpointKind::ChangeRequest,
                    mode: ApprovalMode::Shadow,
                    decide_with: DecideWith::default(),
                    matches_in_a_row: 4,
                    misses: 1,
                    suggest_auto: true,
                }],
                spot_checks_waiting: 2,
                approver: None,
                warnings: Vec::new(),
            },
            notes: vec!["CI failures wait for a CI integration.".to_owned()],
        };
        let text = report_text(&report);
        assert!(
            text.starts_with("change request: shadow, decided by "),
            "{text}"
        );
        assert!(
            text.contains("agreed with you 4 times in a row, 1 miss · ready for auto"),
            "{text}"
        );
        assert!(text.contains("2 spot checks waiting for you."), "{text}");
        assert!(
            text.ends_with("CI failures wait for a CI integration.\n"),
            "{text}"
        );
    }

    #[test]
    fn with_no_record_it_says_so() {
        let report = ApprovalsReport {
            view: ApprovalsView::default(),
            notes: Vec::new(),
        };
        assert_eq!(report_text(&report), "No approval record yet.\n");
    }
}
