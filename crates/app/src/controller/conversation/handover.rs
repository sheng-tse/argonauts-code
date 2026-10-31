//! Where work leaves the main session. Every run the conversation starts, whoever asked for it
//! (a tool call, a command, a question you answered, a verdict), goes to the engine through
//! [`start`]. It is the one call to the entry point that runs the chosen workflow, so whatever
//! runs workflows next is pointed at here and nowhere else.

use argonauts_core::runs::api::StartRun;

use crate::controller::Effect;

/// Starts the workflow in use on `request`: the crew's when its mode is the crew, and a checked
/// Captain otherwise.
pub(super) fn start(request: StartRun) -> Effect {
    Effect::StartRun(Box::new(request))
}
