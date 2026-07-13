//! Shared Client request construction from Agent state.
//!
//! [`build_chat_request`] renders the provider-neutral [`ChatRequest`] the
//! sans-io [`DefaultAgentMachine`](crate::agent::DefaultAgentMachine) sends to
//! the model.
//! It reads only data-only [`AgentState`] facts (the head-clipped committed
//! view, the frozen pending context, the current model, and the system prompt
//! overlay) plus a caller-supplied list of tool declarations. The tool
//! declarations are passed as plain [`Tool`] data rather than a live registry so
//! the sans-io machine can build a request without holding any runtime handle.

use crate::{agent::AgentState, client::ChatRequest, model::tool::Tool};

/// Builds one [`ChatRequest`] from Agent state and the tools to advertise.
///
/// `tools` is the declaration list to expose to the model — the legacy loop
/// passes its live registry's declarations, while the sans-io machine passes the
/// state's current tool set. `stream` selects the transport flag on the request.
pub(crate) fn build_chat_request(
    state: &AgentState,
    tools: Vec<Tool>,
    stream: bool,
) -> ChatRequest {
    let effective = state.conversation().effective_view();
    let (system, mut messages) = effective.into_parts();
    if let Some(pending) = state.conversation().pending_context() {
        messages.extend(pending.into_messages());
    }
    let model = state.current_model();

    ChatRequest {
        model: model.model().to_owned(),
        messages,
        tools,
        system: combine_system_prompt(
            system.or_else(|| state.spec().system_prompt().map(ToOwned::to_owned)),
            state.system_prompt_overlay(),
        ),
        max_tokens: model.max_tokens().get(),
        temperature: model.temperature(),
        stream,
        provider_extras: model.provider_extras().cloned(),
    }
}

/// Combines a base system prompt with an optional overlay appended below it.
fn combine_system_prompt(base: Option<String>, overlay: Option<&str>) -> Option<String> {
    match (base, overlay) {
        (Some(base), Some(overlay)) => Some(format!("{base}\n\n{overlay}")),
        (None, Some(overlay)) => Some(overlay.to_owned()),
        (base, None) => base,
    }
}
