//! A job nothing else can take, and the run can't do without, may go to another model of a
//! CLI already at work in its step, as `fallback.same_step` lets: at once and said in one
//! line, or after you said yes. Two answers from one CLI are less independent than two from
//! two, which is why it comes last, just before the run stops.

use argonauts_core::base::names::ModelRef;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalPacket, FreeText, SecondAgentPacket,
};
use argonauts_core::safety::policy::EvidenceFacts;
use argonauts_runner::{AgentCall, AgentEnd};

use super::{Bridge, lock};
use crate::approvals::{self, ApprovalRequest, autopilot};
use crate::engine::error::{EngineError, Stall};
use crate::engine::job_fallback;
use crate::roles::display_name;

const YES: &str = "yes";
const NO: &str = "no";

impl Bridge<'_> {
    /// Runs `call`'s job on the first other model of a CLI at work in its step that may take
    /// it, once for the job. The stall back when none may, or you said no, which it then
    /// names among what was tried.
    pub(super) async fn beside(
        &self,
        call: &AgentCall,
        mut stall: Box<Stall>,
    ) -> Result<AgentEnd, Box<Stall>> {
        let Some(taker) = stall
            .beside
            .as_ref()
            .and_then(|beside| beside.models.first())
            .cloned()
        else {
            return Err(stall);
        };
        if !lock(&self.second_tried).insert(call.label.clone()) {
            return Err(stall);
        }
        let cli = DisplayNames::default().cli(taker.agent.as_str());
        let second = if self.is_council() {
            format!("a second proposal from {cli}")
        } else {
            format!("a second agent from {cli} in this step")
        };
        let taker_name =
            job_fallback::model_name(self.host, &taker).unwrap_or_else(|| display_name(&taker));
        let asks = stall.beside.as_ref().is_some_and(|beside| beside.ask)
            && !autopilot::applies_now(self.host, ApprovalKind::Question);
        if asks {
            match self.ask(call, &stall, &taker, &taker_name, &second).await {
                Ok(true) => {}
                Ok(false) => {
                    stall.in_step.retain(|working| *working != cli);
                    stall.tried.push(format!("you said no to {second}"));
                    return Err(stall);
                }
                Err(err) => return Ok(self.failed(err)),
            }
        }
        let line = format!(
            "{} runs on {taker_name}, {second}: {} and nothing else can take it",
            call.label, stall.job.said
        );
        if let Err(err) = job_fallback::note(self.host, None, line) {
            return Ok(self.failed(err));
        }
        let mut on_it = call.clone();
        on_it.model = Some(taker.to_string());
        let place = (call.phase.clone(), call.role);
        lock(&self.phases)
            .entry(place.clone())
            .or_default()
            .working
            .insert(call.label.clone());
        let ran = self.run_agent(&on_it).await;
        {
            let mut phases = lock(&self.phases);
            let agents = phases.entry(place).or_default();
            agents.working.remove(&call.label);
            if matches!(&ran, Ok(end) if end.result.is_ok()) {
                agents.answered.insert(call.label.clone());
            }
        }
        match ran {
            Ok(end) => Ok(end),
            // Its own failure is one more thing tried; the job's first one still says why.
            Err(EngineError::Stalled(again)) => {
                stall
                    .tried
                    .push(format!("{taker_name}: {}", again.job.said));
                Err(stall)
            }
            Err(err) => Ok(self.failed(err)),
        }
    }

    /// Asks whether `taker` may take `call`'s job as a second agent of its CLI in the step.
    /// Yours whatever the approval rules say, as only you can weigh the step's independence.
    async fn ask(
        &self,
        call: &AgentCall,
        stall: &Stall,
        taker: &ModelRef,
        taker_name: &str,
        second: &str,
    ) -> Result<bool, EngineError> {
        let text = format!(
            "{} can't run: {} and nothing else can take it. {taker_name} can make it, as \
             {second}, or the run stops here and /resume carries it on.",
            call.label, stall.job.said
        );
        let option = |key, label: String, value: &str, recommended| AnswerOption {
            key,
            label,
            action: AnswerAction::Choose {
                value: value.to_owned(),
            },
            recommended,
            detail: None,
        };
        let request = ApprovalRequest {
            at: Some(self.call.at.clone()),
            kind: ApprovalKind::Question,
            title: format!(
                "{} for {}?",
                argonauts_core::base::text::sentence_case(second),
                call.label
            ),
            task: None,
            packet: ApprovalPacket::SecondAgent(SecondAgentPacket {
                job: call.label.clone(),
                stopped: stall
                    .job
                    .asked
                    .clone()
                    .unwrap_or_else(|| ModelRef::new("", None)),
                taker: taker.clone(),
                text,
            }),
            facts: EvidenceFacts::default(),
            agent_outputs: vec![format!("candidate {taker}")],
            documents: Vec::new(),
            options: Some(vec![
                option('y', format!("Yes, {taker_name} makes it"), YES, true),
                option('n', "No, stop the run".to_owned(), NO, false),
            ]),
            free_text: FreeText::None,
            authors: Vec::new(),
        };
        let decided = approvals::open_and_wait_yours(
            self.host,
            request,
            "fallback.same_step asks you".into(),
        )
        .await?;
        Ok(matches!(&decided.action, AnswerAction::Choose { value } if value == YES))
    }
}
