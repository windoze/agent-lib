//! Terminal response reconciliation, metadata emission, and error mapping.

use super::{StreamNormalizer, invalid_stream};
use crate::{
    adapter::{common, openai_resp::response::parse_response_value},
    client::ClientError,
    stream::StreamEvent,
};
use serde_json::Value;

use super::super::wire::ResponseEvent;

/// Escape-hatch key for future Responses event payloads observed before the
/// terminal response snapshot.
const UNMODELED_STREAM_EVENTS_KEY: &str = "openai_unmodeled_stream_events";

impl StreamNormalizer {
    /// Converts the terminal response snapshot using the same complete-response
    /// path as non-streaming calls, after checking every streamed item.
    pub(super) fn finish_response(
        &mut self,
        event: ResponseEvent,
        expected_status: &str,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.validate_response_snapshot(&event.response, expected_status)?;
        self.validate_terminal_output(&event.response)?;
        // Item state is only needed to validate the terminal snapshot. Drop it
        // before parsing the full response so `output_item.done` payloads do not
        // remain live alongside the parsed terminal response.
        self.items.clear();
        self.item_indices.clear();
        let mut response = parse_response_value(event.response)?;
        if !self.unmodeled_events.is_empty() {
            common::insert_preserving_collision(
                &mut response.extra,
                UNMODELED_STREAM_EVENTS_KEY,
                Value::Array(std::mem::take(&mut self.unmodeled_events)),
            );
        }
        self.terminal = true;

        let mut events = vec![StreamEvent::Usage(response.usage)];
        if !response.extra.is_empty() {
            events.push(StreamEvent::ResponseMetadata {
                extra: response.extra,
            });
        }
        events.push(StreamEvent::MessageStop {
            stop_reason: response.stop_reason,
        });
        Ok(events)
    }

    /// Classifies a terminal failed response as a normalized error event.
    pub(super) fn fail_response(
        &mut self,
        event: ResponseEvent,
        raw_event: Value,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.validate_response_snapshot(&event.response, "failed")?;
        self.terminal = true;
        Ok(vec![StreamEvent::Error(classify_provider_error(
            &raw_event,
        ))])
    }

    /// Ensures one response snapshot belongs to the active response and has
    /// the lifecycle status implied by its event kind.
    pub(super) fn validate_response_snapshot(
        &self,
        response: &Value,
        expected_status: &str,
    ) -> Result<(), ClientError> {
        self.require_started("response lifecycle event")?;
        let id = validate_response_object(response, Some(expected_status))?;
        let started = self
            .response_id
            .as_deref()
            .expect("require_started ensures response id exists");
        if id != started {
            return Err(invalid_stream(format!(
                "response snapshot id `{id}` disagrees with response.created id `{started}`"
            )));
        }
        Ok(())
    }

    /// Compares terminal `response.output` entries with every item-done value.
    fn validate_terminal_output(&self, response: &Value) -> Result<(), ClientError> {
        let fields = response
            .as_object()
            .expect("validated response snapshot must be an object");
        let output = match fields.get("output") {
            Some(Value::Array(output)) => output,
            Some(_) => {
                return Err(invalid_stream(
                    "terminal response field `output` must be an array".to_owned(),
                ));
            }
            None => {
                return Err(invalid_stream(
                    "terminal response field `output` is required".to_owned(),
                ));
            }
        };
        if output.len() != self.items.len() {
            return Err(invalid_stream(format!(
                "terminal response has {} output items but {} were streamed",
                output.len(),
                self.items.len()
            )));
        }
        for (index, terminal_item) in output.iter().enumerate() {
            let index = u64::try_from(index)
                .map_err(|_| invalid_stream("terminal output is too large".to_owned()))?;
            let item = self.items.get(&index).ok_or_else(|| {
                invalid_stream(format!(
                    "terminal response contains unstarted output index {index}"
                ))
            })?;
            item.validate_terminal_item(terminal_item)?;
        }
        Ok(())
    }
}

/// Validates a response object's identity and optional lifecycle status.
pub(super) fn validate_response_object(
    response: &Value,
    expected_status: Option<&str>,
) -> Result<String, ClientError> {
    let fields = response
        .as_object()
        .ok_or_else(|| invalid_stream("response snapshot must be an object".to_owned()))?;
    match fields.get("object") {
        Some(Value::String(object)) if object == "response" => {}
        Some(Value::String(object)) => {
            return Err(invalid_stream(format!(
                "response snapshot object must be `response`, got `{object}`"
            )));
        }
        Some(_) => {
            return Err(invalid_stream(
                "response snapshot field `object` must be a string".to_owned(),
            ));
        }
        None => {
            return Err(invalid_stream(
                "response snapshot field `object` is required".to_owned(),
            ));
        }
    }
    let id = match fields.get("id") {
        Some(Value::String(id)) => id.clone(),
        Some(_) => {
            return Err(invalid_stream(
                "response snapshot field `id` must be a string".to_owned(),
            ));
        }
        None => {
            return Err(invalid_stream(
                "response snapshot field `id` is required".to_owned(),
            ));
        }
    };
    if let Some(expected) = expected_status {
        match fields.get("status") {
            Some(Value::String(status)) if status == expected => {}
            Some(Value::String(status)) => {
                return Err(invalid_stream(format!(
                    "response snapshot status `{status}` disagrees with event status `{expected}`"
                )));
            }
            Some(_) => {
                return Err(invalid_stream(
                    "response snapshot field `status` must be a string".to_owned(),
                ));
            }
            None => {
                return Err(invalid_stream(
                    "response snapshot field `status` is required".to_owned(),
                ));
            }
        }
    }
    Ok(id)
}

/// Checks that `response.created` contains no already-generated output or
/// usage that would bypass incremental events.
pub(super) fn validate_created_placeholders(response: &Value) -> Result<(), ClientError> {
    let fields = response
        .as_object()
        .expect("validated response snapshot must be an object");
    match fields.get("output") {
        Some(Value::Array(output)) if output.is_empty() => {}
        Some(Value::Array(_)) => {
            return Err(invalid_stream(
                "response.created output must be an empty array".to_owned(),
            ));
        }
        Some(_) => {
            return Err(invalid_stream(
                "response.created output must be an array".to_owned(),
            ));
        }
        None => {
            return Err(invalid_stream(
                "response.created output is required".to_owned(),
            ));
        }
    }
    if let Some(usage) = fields.get("usage")
        && !usage.is_null()
    {
        return Err(invalid_stream(
            "response.created usage must be null".to_owned(),
        ));
    }
    Ok(())
}

/// Classifies a provider error event or failed response while retaining its
/// full JSON payload for generic API errors.
pub(super) fn classify_provider_error(raw: &Value) -> ClientError {
    let body = serde_json::to_string(raw).unwrap_or_else(|_| raw.to_string());
    let error = raw
        .as_object()
        .and_then(|fields| fields.get("error"))
        .or_else(|| {
            raw.as_object()
                .and_then(|fields| fields.get("response"))
                .and_then(Value::as_object)
                .and_then(|response| response.get("error"))
        });
    let code = error
        .and_then(Value::as_object)
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .or_else(|| {
            raw.as_object()
                .and_then(|fields| fields.get("code"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
        .to_ascii_lowercase();

    match code.as_str() {
        "rate_limit_exceeded" | "rate_limit_error" => {
            ClientError::RateLimited { retry_after: None }
        }
        "authentication_error" | "invalid_api_key" | "permission_denied" => ClientError::Auth,
        "timeout" | "request_timeout" => ClientError::Timeout,
        "server_error" | "internal_server_error" => ClientError::Api { status: 500, body },
        _ => ClientError::from_http_response(400, body, None),
    }
}
