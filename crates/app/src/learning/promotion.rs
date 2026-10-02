//! The promotion checkpoint: a candidate that cleared its replays and its holdout reaches the
//! decision point `approvals.recipe_promotion`, decided by your approval policy from
//! `PromotionFacts` (its change class and when the recipe last promoted), then the loop's
//! `Evolution::decide`. Anything the rules don't settle waits for you in `/recipes` and the
//! inbox; shadow records what auto would have done and waits too.

use argonauts_core::RunId;
use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::events::run::Party;
use argonauts_core::names::CheckpointId;
use argonauts_core::policy::{
    ApprovalPolicy, DecisionPoint, EvidenceDocument, EvidenceFacts, EvidencePacket, PolicyOutcome,
    PromotionFacts, Verdict,
};
use argonauts_learning::evolution::{Action, Candidate, Evolution};

/// What the checkpoint came to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Promotion {
    /// Your rules or the approver approved it, and the loop promoted it into its canary.
    Promoted { by: Party, actions: Vec<Action> },
    /// Your rules rejected it.
    Rejected { by: Party, actions: Vec<Action> },
    /// It waits for you, with why. `shadow` is what auto would have done.
    Waits {
        reasons: Vec<String>,
        shadow: Option<Verdict>,
    },
}

/// Evaluates the checkpoint for `candidate` and, when the policy decides, tells the loop.
/// `packet` is the comparison the loop handed over, shown as evidence.
pub(crate) async fn checkpoint(
    policy: &dyn ApprovalPolicy,
    evolution: &Evolution,
    candidate: &Candidate,
    packet: &serde_json::Value,
) -> anyhow::Result<Promotion> {
    let (point, evidence) = decision(evolution, candidate, packet)?;
    let outcome = policy.evaluate(&point, &evidence).await;
    let (verdict, by, reasons) = match outcome {
        PolicyOutcome::AskUser { violations, shadow } => {
            let mut reasons: Vec<String> = violations
                .iter()
                .map(|violation| violation.detail.clone())
                .collect();
            if let Some(shadow) = &shadow {
                reasons.extend(shadow.reasons.iter().cloned());
            }
            return Ok(Promotion::Waits {
                reasons,
                shadow: shadow.map(|shadow| shadow.would),
            });
        }
        PolicyOutcome::Decided {
            verdict,
            approver,
            reasons,
            ..
        } => {
            let by = match approver {
                Some(model) => Party::Approver { model },
                None => Party::Rules,
            };
            (verdict, by, reasons)
        }
    };
    match verdict {
        Verdict::Approve => Ok(Promotion::Promoted {
            actions: evolution.decide(&candidate.id, true, &by)?,
            by,
        }),
        Verdict::Reject => Ok(Promotion::Rejected {
            actions: evolution.decide(&candidate.id, false, &by)?,
            by,
        }),
        Verdict::Escalate => Ok(Promotion::Waits {
            reasons,
            shadow: None,
        }),
    }
}

/// The decision point and its evidence. Facts come from the loop's own records, never from
/// what an agent said.
fn decision(
    evolution: &Evolution,
    candidate: &Candidate,
    packet: &serde_json::Value,
) -> anyhow::Result<(DecisionPoint, EvidencePacket)> {
    let run = match &candidate.from_run {
        Some(run) => run.clone(),
        None => RunId::new("evolution")?,
    };
    // Its own id, so the approval records never mistake it for a checkpoint of the run that
    // proposed it, or for another candidate's.
    let checkpoint = CheckpointId::parse(&format!("promote-{}", candidate.id))
        .ok_or_else(|| anyhow::anyhow!("{} can't name a checkpoint", candidate.id))?;
    let point = DecisionPoint {
        run,
        checkpoint,
        kind: CheckpointKind::RecipePromotion,
        recipe: Some(candidate.parent.name.clone()),
        authors: Vec::new(),
        mode_override: None,
    };
    let facts = EvidenceFacts {
        promotion: Some(PromotionFacts {
            change_class: crate::learning::view::serde_word(&candidate.change.class),
            last_promoted: super::or_empty(evolution.last_promoted(&candidate.parent.name))?,
        }),
        ..EvidenceFacts::default()
    };
    let evidence = EvidencePacket {
        kind: CheckpointKind::RecipePromotion,
        goal: None,
        facts,
        agent_outputs: Vec::new(),
        documents: vec![EvidenceDocument {
            label: format!("{} v{}", candidate.parent.name, candidate.version),
            text: serde_json::to_string_pretty(packet)?,
            path: None,
            sha256: None,
        }],
    };
    Ok((point, evidence))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use time::OffsetDateTime;

    use argonauts_core::events::card::CardSpec;
    use argonauts_core::future::BoxFuture;
    use argonauts_core::policy::{
        ApprovalsView, HardLimits, ShadowRecord, UserDecision, Violation,
    };
    use argonauts_core::settings::Settings;
    use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
    use argonauts_core::worker::SessionSpec;
    use argonauts_learning::evolution::{CandidateState, Thresholds, state};
    use argonauts_learning::recipes::{Change, ChangeClass};
    use argonauts_learning::{RecipeRef, Store};

    use super::*;

    /// A policy that answers every point with `outcome` and keeps what it was asked.
    struct Answering {
        outcome: PolicyOutcome,
        asked: Mutex<Vec<(DecisionPoint, EvidencePacket)>>,
    }

    impl HardLimits for Answering {
        fn check_point(&self, _: &DecisionPoint, _: &EvidencePacket) -> Vec<Violation> {
            Vec::new()
        }
        fn check_card_diff(&self, _: &CardSpec, _: &str) -> Vec<Violation> {
            Vec::new()
        }
        fn check_session(&self, _: &SessionSpec) -> Vec<Violation> {
            Vec::new()
        }
    }

    impl ApprovalPolicy for Answering {
        fn evaluate<'a>(
            &'a self,
            point: &'a DecisionPoint,
            evidence: &'a EvidencePacket,
        ) -> BoxFuture<'a, PolicyOutcome> {
            if let Ok(mut asked) = self.asked.lock() {
                asked.push((point.clone(), evidence.clone()));
            }
            Box::pin(async move { self.outcome.clone() })
        }
        fn user_decided(&self, _: &UserDecision) {}
        fn spot_checked(&self, _: &DecisionPoint, _: bool) {}
        fn auto_off(&self) {}
        fn hard_limits(&self) -> &dyn HardLimits {
            self
        }
        fn view(&self) -> ApprovalsView {
            ApprovalsView::default()
        }
    }

    fn answering(outcome: PolicyOutcome) -> Answering {
        Answering {
            outcome,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn candidate() -> Candidate {
        Candidate {
            id: "api-feature-v5".to_owned(),
            parent: RecipeRef {
                name: "api-feature".to_owned(),
                version: 4,
            },
            version: 5,
            change: Change {
                key: "critique.checklist".to_owned(),
                class: ChangeClass::ChecklistItem,
                before: None,
                after: Some(serde_json::json!(["criteria", "rate_limits"])),
            },
            why: "Two runs missed rate limits.".to_owned(),
            from_run: Some(RunId::new("run-07").unwrap()),
            thresholds: Thresholds {
                min_gain_points: 10.0,
                max_cost_increase: 0.1,
                not_worse: false,
            },
            state: CandidateState::AwaitingPromotion,
            queued: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// The loop's own record of `candidates`, which `decide` reads.
    fn stored(store: &Store, candidates: &[Candidate]) {
        state::write(
            &store.evolution_dir().join(state::CANDIDATES),
            &candidates.to_vec(),
        )
        .unwrap();
    }

    fn decided(verdict: Verdict) -> PolicyOutcome {
        PolicyOutcome::Decided {
            verdict,
            by: DecideWith::Rules,
            approver: None,
            reasons: vec!["checklist_item is a class you allow".to_owned()],
            spot_check: false,
        }
    }

    /// Your real approval policy over `settings`, keeping its records under `state`.
    fn real_policy(settings: Settings, state: &Path) -> Arc<dyn ApprovalPolicy> {
        let settings = Arc::new(settings);
        let reload: argonauts_policy::SettingsReloader = {
            let settings = settings.clone();
            Arc::new(move || Some((*settings).clone()))
        };
        argonauts_policy::approval_policy(
            settings,
            reload,
            state,
            Arc::new(crate::commands::NoWorkers),
            Arc::new(argonauts_core::steps::SchemaStepRunner),
        )
    }

    /// The decision follows the policy: what it settles goes to the loop, what it doesn't
    /// waits for you, and the real policy under supervised or shadow never promotes.
    #[tokio::test]
    async fn promotion_checkpoint_follows_approvals() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("state"), dir.path().join("repo"));
        stored(&store, &[candidate()]);
        let evolution = Evolution::open(&store).unwrap();
        let packet = serde_json::json!({"gain_points": 14.0});

        // The policy is asked at the recipe promotion point, with the change class as a fact.
        let auto = answering(decided(Verdict::Approve));
        let outcome = checkpoint(&auto, &evolution, &candidate(), &packet)
            .await
            .unwrap();
        let asked = auto.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        let (point, evidence) = &asked[0];
        assert_eq!(point.kind, CheckpointKind::RecipePromotion);
        assert_eq!(point.recipe.as_deref(), Some("api-feature"));
        assert_eq!(point.checkpoint.as_str(), "promote-api-feature-v5");
        assert_eq!(
            evidence
                .facts
                .promotion
                .as_ref()
                .map(|p| p.change_class.as_str()),
            Some("checklist_item")
        );
        // An approval goes on to the loop's decision, which starts the canary.
        assert!(
            matches!(
                &outcome,
                Promotion::Promoted {
                    by: Party::Rules,
                    ..
                }
            ),
            "{outcome:?}"
        );
        let canary = evolution.canaries().unwrap();
        assert_eq!(
            canary.get("api-feature").map(|c| c.candidate.as_str()),
            Some("api-feature-v5")
        );
        assert_eq!(
            evolution
                .candidate("api-feature-v5")
                .unwrap()
                .map(|c| c.state),
            Some(CandidateState::Canary { runs: 0 })
        );

        // Escalated, or asked of you, it waits, with what shadow would have done.
        let escalated = answering(decided(Verdict::Escalate));
        let waits = checkpoint(&escalated, &evolution, &candidate(), &packet)
            .await
            .unwrap();
        assert!(matches!(waits, Promotion::Waits { shadow: None, .. }));
        let shadowed = answering(PolicyOutcome::AskUser {
            violations: Vec::new(),
            shadow: Some(ShadowRecord {
                would: Verdict::Approve,
                by: DecideWith::Rules,
                reasons: vec!["would promote".to_owned()],
            }),
        });
        let waits = checkpoint(&shadowed, &evolution, &candidate(), &packet)
            .await
            .unwrap();
        assert_eq!(
            waits,
            Promotion::Waits {
                reasons: vec!["would promote".to_owned()],
                shadow: Some(Verdict::Approve),
            }
        );

        // Your real policy, set to supervised or shadow, never promotes on its own.
        for mode in [ApprovalMode::Supervised, ApprovalMode::Shadow] {
            let mut settings = Settings::default();
            settings.approvals.recipe_promotion.mode = mode;
            let policy = real_policy(settings, &dir.path().join("state"));
            let outcome = checkpoint(policy.as_ref(), &evolution, &candidate(), &packet)
                .await
                .unwrap();
            assert!(
                matches!(outcome, Promotion::Waits { .. }),
                "{mode:?}: {outcome:?}"
            );
        }
    }

    /// With the defaults, your rules promote a class they allow on their own, then hold the
    /// recipe's next promotion for `min_days_between` days from when this one started, however
    /// long the next candidate waited in the queue. A class they don't allow waits for you.
    #[tokio::test]
    async fn default_rules_promote_allowed_classes_and_space_them_out() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let store = Store::new(&state_dir, dir.path().join("repo"));
        let packet = serde_json::json!({"gain_points": 14.0});
        stored(&store, &[candidate()]);
        let evolution = Evolution::open(&store).unwrap();
        let policy = real_policy(Settings::default(), &state_dir);

        let promoted = checkpoint(policy.as_ref(), &evolution, &candidate(), &packet)
            .await
            .unwrap();
        assert!(
            matches!(
                &promoted,
                Promotion::Promoted {
                    by: Party::Rules,
                    ..
                }
            ),
            "{promoted:?}"
        );
        assert!(evolution.canaries().unwrap().contains_key("api-feature"));
        let started = evolution.last_promoted("api-feature").unwrap();
        assert!(started.is_some());

        // The canary ends, and the recipe's next candidate, queued long ago, comes up.
        std::fs::remove_file(store.evolution_dir().join(state::CANARIES)).unwrap();
        let next = Candidate {
            id: "api-feature-v6".to_owned(),
            version: 6,
            ..candidate()
        };
        let mut all = evolution.candidates().unwrap();
        all.push(next.clone());
        stored(&store, &all);
        let waits = checkpoint(policy.as_ref(), &evolution, &next, &packet)
            .await
            .unwrap();
        match &waits {
            Promotion::Waits { reasons, .. } => assert!(
                reasons.iter().any(|reason| reason.contains("14 days")),
                "{reasons:?}"
            ),
            other => panic!("{other:?}"),
        }
        assert_eq!(evolution.last_promoted("api-feature").unwrap(), started);

        // A class your rules don't allow, on a recipe that never promoted, waits too.
        let workflow = Candidate {
            id: "cli-flag-v2".to_owned(),
            parent: RecipeRef {
                name: "cli-flag".to_owned(),
                version: 1,
            },
            version: 2,
            change: Change {
                key: "workflow".to_owned(),
                class: ChangeClass::StepGraph,
                before: None,
                after: Some(serde_json::json!("solo-reviewed")),
            },
            ..candidate()
        };
        all.push(workflow.clone());
        stored(&store, &all);
        let waits = checkpoint(policy.as_ref(), &evolution, &workflow, &packet)
            .await
            .unwrap();
        assert!(matches!(waits, Promotion::Waits { .. }), "{waits:?}");
        assert!(!evolution.canaries().unwrap().contains_key("cli-flag"));
    }
}
