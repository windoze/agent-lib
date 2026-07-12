//! OpenAI Responses request serialization and HTTP request construction.

use super::OpenAiRespAdapter;
use crate::{
    client::{AuthScheme, ChatRequest, ClientError, EndpointConfig},
    model::extras::{ProviderExtrasMergeOutcome, ProviderId},
};
use reqwest::{
    Request, Url,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde::Serialize;
use serde_json::Value;

mod input;

use input::{message_to_items, tool_to_wire};

impl OpenAiRespAdapter {
    /// Builds a `POST /responses` request without sending it.
    ///
    /// Provider-neutral messages are expanded into Responses input items,
    /// matching provider extras are merged at the final JSON boundary, and
    /// endpoint authentication, headers, and query parameters are applied to
    /// the buffered reqwest request.
    pub fn build_request(&self, request: &ChatRequest) -> Result<Request, ClientError> {
        let body = serialize_body(request)?;
        let url = responses_url(&self.endpoint)?;
        let headers = endpoint_headers(&self.endpoint)?;

        self.http_client
            .post(url)
            .headers(headers)
            .json(&body)
            .build()
            .map_err(|error| invalid_endpoint(format!("failed to build HTTP request: {error}")))
    }
}

/// OpenAI's top-level Responses body before provider extras are merged.
#[derive(Serialize)]
struct OpenAiResponseRequestBody<'a> {
    model: &'a str,
    input: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<&'a str>,
    max_output_tokens: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
}

/// Serializes one normalized request and performs the final extras merge.
fn serialize_body(request: &ChatRequest) -> Result<Value, ClientError> {
    let input = request
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| message_to_items(index, message))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    let tools = request.tools.iter().map(tool_to_wire).collect();
    let wire = OpenAiResponseRequestBody {
        model: &request.model,
        input,
        instructions: request.system.as_deref(),
        max_output_tokens: request.max_tokens,
        tools,
        temperature: request.temperature,
        stream: request.stream,
    };
    let mut body = serde_json::to_value(wire)
        .map_err(|error| invalid_request(format!("failed to serialize request body: {error}")))?;

    if let Some(extras) = &request.provider_extras {
        let outcome = extras
            .merge_into(&mut body, ProviderId::OpenAiResp)
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

/// Parses the configured base URL and appends the Responses endpoint path.
fn responses_url(endpoint: &EndpointConfig) -> Result<Url, ClientError> {
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
        segments.pop_if_empty().push("responses");
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

/// Classifies invalid normalized-to-Responses conversion as a protocol error.
pub(super) fn invalid_request(message: String) -> ClientError {
    ClientError::Protocol(format!("invalid OpenAI Responses request: {message}"))
}

/// Classifies malformed endpoint configuration before any network operation.
fn invalid_endpoint(message: String) -> ClientError {
    ClientError::Other(format!(
        "invalid OpenAI Responses endpoint configuration: {message}"
    ))
}

#[cfg(test)]
mod tests;
