//! Endpoint URL and header construction shared by LLM adapters.

use crate::client::{AuthScheme, ClientError, EndpointConfig};
use reqwest::{
    Url,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};

/// Parses the configured base URL and appends provider endpoint path segments.
///
/// The base URL may already include a proxy prefix (for example `/openai/v1/`).
/// Empty trailing path segments are removed before `path_segments` are appended,
/// fragments are discarded, and configured query parameters are preserved.
pub(crate) fn endpoint_url<F>(
    endpoint: &EndpointConfig,
    path_segments: &[&str],
    invalid_endpoint: F,
) -> Result<Url, ClientError>
where
    F: Fn(String) -> ClientError,
{
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
        segments.pop_if_empty();
        for segment in path_segments {
            segments.push(segment);
        }
    }
    url.set_fragment(None);
    if !endpoint.query_params.is_empty() {
        url.query_pairs_mut()
            .extend_pairs(endpoint.query_params.iter());
    }

    Ok(url)
}

/// Builds validated JSON request headers for every supported auth scheme.
pub(crate) fn endpoint_headers<F>(
    endpoint: &EndpointConfig,
    invalid_endpoint: F,
) -> Result<HeaderMap, ClientError>
where
    F: Fn(String) -> ClientError,
{
    let mut headers = HeaderMap::new();
    match &endpoint.auth {
        AuthScheme::Bearer(token) => {
            append_header(
                &mut headers,
                AUTHORIZATION.as_str(),
                &format!("Bearer {token}"),
                true,
                &invalid_endpoint,
            )?;
        }
        AuthScheme::Header { name, value } => {
            append_header(&mut headers, name, value, true, &invalid_endpoint)?;
        }
        AuthScheme::None => {}
    }

    for (name, value) in &endpoint.extra_headers {
        append_header(&mut headers, name, value, false, &invalid_endpoint)?;
    }
    if !headers.contains_key(CONTENT_TYPE) {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }

    Ok(headers)
}

/// Validates and appends one header, retaining repeated configured fields.
fn append_header<F>(
    headers: &mut HeaderMap,
    name: &str,
    value: &str,
    sensitive: bool,
    invalid_endpoint: &F,
) -> Result<(), ClientError>
where
    F: Fn(String) -> ClientError,
{
    let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|error| invalid_endpoint(format!("invalid header name `{name}`: {error}")))?;
    let mut value = HeaderValue::from_str(value)
        .map_err(|error| invalid_endpoint(format!("invalid value for header `{name}`: {error}")))?;
    value.set_sensitive(sensitive || name == AUTHORIZATION);
    headers.append(name, value);

    Ok(())
}
