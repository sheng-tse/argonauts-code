//! A workflow the conversation's own CLI runs in a chat turn, mirrored as a run's are: a panel
//! row, and the workflow view with whatever phases and agents it reports. The views name the
//! conversation by [`conversation_run`], so a row and a refresh find it.

use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::events::session::VendorWorkflow;
use argonauts_core::names::WorkflowId;
use argonauts_core::views::{
    AgentRow, RowStatus, WorkflowView, conversation_run, vendor_workflow_id,
};
use time::OffsetDateTime;

use super::ChatState;
use crate::controller::Ctx;

/// The newest report of each workflow, its time counted from when it was first seen here.
pub(super) fn seen(workflows: &mut Vec<VendorWorkflow>, report: &VendorWorkflow) {
    let mut report = report.clone();
    let now = now_ms();
    match workflows.iter_mut().find(|known| known.id == report.id) {
        Some(known) => {
            report.started_at_ms = known.started_at_ms.or(report.started_at_ms).or(now);
            *known = report;
        }
        None => {
            report.started_at_ms = report.started_at_ms.or(now);
            workflows.push(report);
        }
    }
}

/// The view of `id`, or of the newest workflow when `None`.
pub(crate) fn show(ctx: &Ctx, chat: &ChatState, id: Option<&WorkflowId>) {
    let workflow = match id {
        Some(id) => chat
            .workflows
            .iter()
            .find(|workflow| vendor_workflow_id(&workflow.id) == *id),
        None => chat.workflows.last(),
    };
    let (Some(workflow), Some(context)) = (workflow, chat.context.as_ref()) else {
        // Only a key asks for this view, so the answer goes on the notice line.
        ctx.hint("This conversation runs no workflow now. Ctrl+T shows one once it starts.");
        return;
    };
    let host = AgentRow {
        session: context.session.clone(),
        label: context.label.clone(),
        worker: context.worker.clone(),
        status: if workflow.ended() {
            RowStatus::Done
        } else {
            RowStatus::Running
        },
        tokens: context.used,
        spent: context.spent,
        elapsed_ms: 0,
        subagents: 0,
        context: None,
        standing_in_for: None,
        answered: None,
        tries: Vec::new(),
    };
    let mut view = WorkflowView::of_vendor(&conversation_run(), &host, workflow, now_ms());
    crate::views::required::mark(&mut view, &ctx.env, super::talk::conversation_steps(chat));
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Workflow {
            view: Box::new(view),
        }),
    });
}

fn now_ms() -> Option<u64> {
    u64::try_from((OffsetDateTime::now_utc() - OffsetDateTime::UNIX_EPOCH).whole_milliseconds())
        .ok()
}
