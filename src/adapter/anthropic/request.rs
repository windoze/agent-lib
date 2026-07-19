//! Anthropic Messages request serialization and HTTP request construction.

use super::AnthropicAdapter;
use crate::{
    client::{AuthScheme, ChatRequest, ClientError, EndpointConfig},
    model::{
        content::{ContentBlock, ImageSource},
        extras::{ProviderExtrasMergeOutcome, ProviderId},
        message::{Message, Role},
        tool::{Tool, ToolStatus},
    },
};
use reqwest::{
    Request, Url,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde::Serialize;
use serde_json::{Map, Value};

impl AnthropicAdapter {
    /// Builds a `POST /v1/messages` request without sending it.
    ///
    /// This is the final serialization boundary: provider-neutral fields are
    /// translated to Anthropic wire names, matching Anthropic provider extras
    /// are merged last, and endpoint authentication, headers, and query
    /// parameters are applied to the resulting reqwest request.
    pub fn build_request(&self, request: &ChatRequest) -> Result<Request, ClientError> {
        let body = serialize_body(request)?;
        let url = messages_url(&self.endpoint)?;
        let headers = endpoint_headers(&self.endpoint)?;

        self.http_client
            .post(url)
            .headers(headers)
            .json(&body)
            .build()
            .map_err(|error| invalid_endpoint(format!("failed to build HTTP request: {error}")))
    }
}

/// Anthropic's top-level Messages request body before provider extras merge.
#[derive(Serialize)]
struct AnthropicRequestBody<'a> {
    model: &'a str,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "slice_is_empty")]
    tools: &'a [Tool],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

/// A role-constrained Anthropic Messages input item.
#[derive(Serialize)]
struct AnthropicMessage {
    role: AnthropicRole,
    content: Vec<Value>,
}

/// Roles accepted in the Anthropic `messages` array.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum AnthropicRole {
    User,
    Assistant,
}

/// Serializes one normalized request and performs the final extras merge.
fn serialize_body(request: &ChatRequest) -> Result<Value, ClientError> {
    let messages = request
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| message_to_wire(index, message))
        .collect::<Result<Vec<_>, _>>()?;
    let wire = AnthropicRequestBody {
        model: &request.model,
        messages,
        system: request.system.as_deref(),
        max_tokens: request.max_tokens,
        tools: &request.tools,
        temperature: request.temperature,
        stream: request.stream,
    };
    let mut body = serde_json::to_value(wire)
        .map_err(|error| invalid_request(format!("failed to serialize request body: {error}")))?;

    if let Some(extras) = &request.provider_extras {
        let outcome = extras
            .merge_into(&mut body, ProviderId::Anthropic)
            .map_err(|error| invalid_request(error.to_string()))?;
        if let ProviderExtrasMergeOutcome::IgnoredProviderMismatch {
            extras_provider,
            target,
        } = outcome
        {
            return Err(invalid_request(format!(
                "provider extras for {extras_provider:?} cannot be sent through {target:?}"
            )));
        }
    }

    Ok(body)
}

/// Converts a provider-neutral message to an Anthropic role and block array.
fn message_to_wire(index: usize, message: &Message) -> Result<AnthropicMessage, ClientError> {
    let role = match message.role {
        Role::User | Role::Tool => AnthropicRole::User,
        Role::Assistant => AnthropicRole::Assistant,
        Role::System => {
            return Err(invalid_request(format!(
                "message {index} has system role; use ChatRequest.system instead"
            )));
        }
    };
    let content = message.content.iter().map(content_to_wire).collect();

    Ok(AnthropicMessage { role, content })
}

/// Converts every complete content-block variant to Anthropic field names.
fn content_to_wire(block: &ContentBlock) -> Value {
    let fields = match block {
        ContentBlock::Text { text, extra } => {
            let mut fields = extra.clone();
            insert_string(&mut fields, "type", "text");
            insert_string(&mut fields, "text", text);
            fields
        }
        ContentBlock::Image { source, extra } => {
            let mut fields = extra.clone();
            insert_string(&mut fields, "type", "image");
            fields.insert("source".to_owned(), image_source_to_wire(source));
            fields
        }
        ContentBlock::ToolUse {
            id,
            name,
            input,
            extra,
        } => {
            let mut fields = extra.clone();
            insert_string(&mut fields, "type", "tool_use");
            insert_string(&mut fields, "id", id);
            insert_string(&mut fields, "name", name);
            fields.insert("input".to_owned(), input.clone());
            fields
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            status,
            extra,
        } => {
            let mut fields = extra.clone();
            insert_string(&mut fields, "type", "tool_result");
            insert_string(&mut fields, "tool_use_id", tool_use_id);
            fields.insert(
                "content".to_owned(),
                Value::Array(content.iter().map(content_to_wire).collect()),
            );
            fields.remove("status");
            if anthropic_tool_result_is_error(*status) {
                fields.insert("is_error".to_owned(), Value::Bool(true));
            } else {
                fields.remove("is_error");
            }
            fields
        }
        ContentBlock::Thinking {
            text,
            signature,
            extra,
        } => {
            let mut fields = extra.clone();
            insert_string(&mut fields, "type", "thinking");
            insert_string(&mut fields, "thinking", text);
            if let Some(signature) = signature {
                insert_string(&mut fields, "signature", signature);
            } else {
                fields.remove("signature");
            }
            fields
        }
        ContentBlock::Unknown { raw, .. } => return raw.clone(),
    };

    Value::Object(fields)
}

/// Degrades the four normalized outcomes to Anthropic's error boolean without
/// changing the source block or inventing a more specific provider status.
fn anthropic_tool_result_is_error(status: ToolStatus) -> bool {
    match status {
        ToolStatus::Ok => false,
        ToolStatus::Error | ToolStatus::Denied | ToolStatus::Cancelled => true,
    }
}

/// Converts URL and base64 image sources while preserving source-level extras.
fn image_source_to_wire(source: &ImageSource) -> Value {
    let fields = match source {
        ImageSource::Url { url, extra } => {
            let mut fields = extra.clone();
            insert_string(&mut fields, "type", "url");
            insert_string(&mut fields, "url", url);
            fields
        }
        ImageSource::Base64 {
            media_type,
            data,
            extra,
        } => {
            let mut fields = extra.clone();
            insert_string(&mut fields, "type", "base64");
            insert_string(&mut fields, "media_type", media_type);
            insert_string(&mut fields, "data", data);
            fields
        }
    };

    Value::Object(fields)
}

/// Parses the configured base URL and appends the Anthropic Messages path.
fn messages_url(endpoint: &EndpointConfig) -> Result<Url, ClientError> {
    let mut url = Url::parse(&endpoint.base_url)
        .map_err(|error| invalid_endpoint(format!("invalid base URL: {error}")))?;
    if url.cannot_be_a_base() {
        return Err(invalid_endpoint(
            "base URL cannot have path segments".to_owned(),
        ));
    }

    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| invalid_endpoint("base URL cannot have path segments".to_owned()))?;
        segments.pop_if_empty().push("v1").push("messages");
    }
    url.set_fragment(None);
    if !endpoint.query_params.is_empty() {
        url.query_pairs_mut()
            .extend_pairs(endpoint.query_params.iter());
    }

    Ok(url)
}

/// Builds validated HTTP headers for every supported authentication scheme.
fn endpoint_headers(endpoint: &EndpointConfig) -> Result<HeaderMap, ClientError> {
    let mut headers = HeaderMap::new();
    match &endpoint.auth {
        AuthScheme::Bearer(token) => {
            append_header(
                &mut headers,
                AUTHORIZATION.as_str(),
                &format!("Bearer {token}"),
                true,
            )?;
        }
        AuthScheme::Header { name, value } => {
            append_header(&mut headers, name, value, true)?;
        }
        AuthScheme::None => {}
    }

    for (name, value) in &endpoint.extra_headers {
        append_header(&mut headers, name, value, false)?;
    }
    if !headers.contains_key(CONTENT_TYPE) {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }

    Ok(headers)
}

/// Validates and appends one header, retaining repeated configured fields.
fn append_header(
    headers: &mut HeaderMap,
    name: &str,
    value: &str,
    sensitive: bool,
) -> Result<(), ClientError> {
    let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|error| invalid_endpoint(format!("invalid header name `{name}`: {error}")))?;
    let mut value = HeaderValue::from_str(value)
        .map_err(|error| invalid_endpoint(format!("invalid value for header `{name}`: {error}")))?;
    value.set_sensitive(sensitive || name == AUTHORIZATION);
    headers.append(name, value);

    Ok(())
}

/// Inserts a normalized string field after extras so modeled data wins.
fn insert_string(fields: &mut Map<String, Value>, key: &str, value: &str) {
    fields.insert(key.to_owned(), Value::String(value.to_owned()));
}

/// Lets serde omit an empty borrowed tool slice from the request body.
fn slice_is_empty<T>(value: &&[T]) -> bool {
    value.is_empty()
}

/// Classifies invalid normalized-to-Anthropic conversion as a protocol error.
fn invalid_request(message: String) -> ClientError {
    ClientError::Protocol(format!("invalid Anthropic Messages request: {message}"))
}

/// Classifies malformed endpoint configuration before any network operation.
fn invalid_endpoint(message: String) -> ClientError {
    ClientError::Other(format!(
        "invalid Anthropic endpoint configuration: {message}"
    ))
}

#[cfg(test)]
mod tests;
