//! Provider-agnostic assertions for normalized complete responses.

use agent_lib::{
    client::Response,
    model::{content::ContentBlock, message::Role, normalized::StopReason},
};

/// Provider-neutral evidence needed to correlate a simulated tool result.
pub(super) struct ObservedToolCall {
    /// Provider-assigned call identifier exposed through the common model.
    pub(super) id: String,
}

/// Checks the structural contract shared by every successful response.
pub(super) fn assert_common_response(
    response: &Response,
    expected_stop: StopReason,
) -> Result<(), String> {
    if response.message.role != Role::Assistant {
        return Err(format!(
            "normalized response role was {:?}, expected Assistant",
            response.message.role
        ));
    }
    if response.message.content.is_empty() {
        return Err("normalized assistant content was empty".to_owned());
    }
    if response.stop_reason.value != expected_stop {
        return Err(format!(
            "normalized stop reason was {:?}, expected {expected_stop:?}",
            response.stop_reason.value
        ));
    }
    if response
        .stop_reason
        .raw
        .as_deref()
        .is_none_or(|raw| raw.trim().is_empty())
    {
        return Err("normalized stop reason did not preserve a raw value".to_owned());
    }
    if response.usage.input == 0 {
        return Err("normalized input usage was zero".to_owned());
    }
    if response.usage.output == 0 {
        return Err("normalized output usage was zero".to_owned());
    }

    Ok(())
}

/// Checks a final text response and required case-insensitive fragments.
pub(super) fn assert_text_response(
    response: &Response,
    required_fragments: &[&str],
) -> Result<(), String> {
    assert_common_response(response, StopReason::EndTurn)?;
    let text = normalized_text(response);
    if text.trim().is_empty() {
        return Err("normalized response contained no non-empty text block".to_owned());
    }

    let lowercase = text.to_lowercase();
    for fragment in required_fragments {
        if !lowercase.contains(&fragment.to_lowercase()) {
            return Err(format!(
                "normalized text did not contain required fragment `{fragment}`: {text:?}"
            ));
        }
    }

    Ok(())
}

/// Checks the first half of a tool round trip and returns its correlation id.
pub(super) fn assert_weather_tool_call(response: &Response) -> Result<ObservedToolCall, String> {
    assert_common_response(response, StopReason::ToolUse)?;
    let calls = response
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse {
                id, name, input, ..
            } => Some((id, name, input)),
            _ => None,
        })
        .collect::<Vec<_>>();

    let [(id, name, input)] = calls.as_slice() else {
        return Err(format!(
            "expected exactly one normalized tool call, observed {}",
            calls.len()
        ));
    };
    if name.as_str() != "get_weather" {
        return Err(format!(
            "normalized tool name was {name:?}, expected get_weather"
        ));
    }
    if input.get("city").and_then(serde_json::Value::as_str) != Some("Tokyo") {
        return Err(format!(
            "normalized get_weather input did not contain city=Tokyo: {input}"
        ));
    }
    if id.is_empty() {
        return Err("normalized tool call id was empty".to_owned());
    }

    Ok(ObservedToolCall { id: (*id).clone() })
}

/// Ensures a completed tool round trip did not request the same tool again.
pub(super) fn assert_no_tool_call(response: &Response) -> Result<(), String> {
    if response
        .message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
    {
        return Err(
            "final normalized response unexpectedly contained another tool call".to_owned(),
        );
    }

    Ok(())
}

/// Concatenates only normalized text blocks, ignoring reasoning and tool data.
fn normalized_text(response: &Response) -> String {
    response
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
