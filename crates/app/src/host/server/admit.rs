//! Who may send a request. Requests that decide or raise autonomy need the run's token, and a
//! peer process that descends from an agent can't send them. A spot check carries no token,
//! but its verdict can raise autonomy too, so its peer is checked all the same.

use argonauts_core::control::ControlRequest;

use super::{Origin, ServerState};

/// `Answer`, `ApprovalMode`, `TakeOver`, `TakeOverEnded`, and a `Reassign` with a rule override.
fn needs_token(request: &ControlRequest) -> bool {
    match request {
        ControlRequest::Answer { .. }
        | ControlRequest::ApprovalMode { .. }
        | ControlRequest::TakeOver { .. }
        | ControlRequest::TakeOverEnded { .. } => true,
        ControlRequest::Reassign { rule_override, .. } => rule_override.is_some(),
        _ => false,
    }
}

fn token_of(request: &ControlRequest) -> Option<&str> {
    match request {
        ControlRequest::Answer { token, .. }
        | ControlRequest::ApprovalMode { token, .. }
        | ControlRequest::TakeOver { token, .. }
        | ControlRequest::TakeOverEnded { token, .. } => Some(token),
        ControlRequest::Reassign { token, .. } => token.as_deref(),
        _ => None,
    }
}

/// Compares every byte, so the time taken says nothing about how much of a guess was right.
fn same_token(given: &str, expected: &str) -> bool {
    given.len() == expected.len()
        && !expected.is_empty()
        && given
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

fn needs_a_person(request: &ControlRequest) -> bool {
    needs_token(request) || matches!(request, ControlRequest::SpotCheck { .. })
}

pub(super) async fn admit(
    state: &ServerState,
    request: &ControlRequest,
    origin: Origin,
) -> Result<(), String> {
    if !needs_a_person(request) {
        return Ok(());
    }
    if needs_token(request) && !same_token(token_of(request).unwrap_or_default(), &state.token) {
        return Err("the approval token is missing or wrong".to_owned());
    }
    let pid = match origin {
        Origin::Pending => return Ok(()),
        Origin::Peer { .. } if !state.check_peers => return Ok(()),
        Origin::Peer { pid: None } => {
            return Err(
                "refused: the sending process can't be identified, so it can't approve".to_owned(),
            );
        }
        Origin::Peer { pid: Some(pid) } => pid,
    };
    let home = state.home.clone();
    let markers = state.markers.clone();
    let check = state.ancestry.clone();
    let found = tokio::task::spawn_blocking(move || check(pid, &home, &markers)).await;
    match found {
        Ok(Ok(None)) => Ok(()),
        Ok(Ok(Some(reason))) => Err(format!(
            "refused: approvals can't come from inside an agent ({reason})"
        )),
        Ok(Err(err)) => Err(format!("refused: {err:#}")),
        Err(err) => Err(format!("refused: the ancestry check failed: {err}")),
    }
}
