//! Serializable endpoint transport configuration.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Placeholder substituted for every credential value in [`Debug`] output.
const REDACTED: &str = "[REDACTED]";

/// Lowercase header-name fragments that mark an extra header as
/// credential-bearing, so its value is redacted in [`Debug`] output. Kept
/// deliberately broad: an over-redacted header list is safe, an under-redacted
/// one leaks secrets.
const SENSITIVE_HEADER_FRAGMENTS: &[&str] =
    &["key", "token", "secret", "auth", "password", "credential"];

/// Returns whether `header_name` looks credential-bearing (for example
/// `api-key`, `x-api-key`, or `authorization`).
fn is_sensitive_header(header_name: &str) -> bool {
    let lower = header_name.to_ascii_lowercase();
    SENSITIVE_HEADER_FRAGMENTS
        .iter()
        .any(|fragment| lower.contains(fragment))
}

/// Authentication applied when sending requests to an LLM endpoint.
///
/// [`AuthScheme::Header`] covers provider-specific names such as `api-key`
/// and `x-api-key` without coupling endpoint configuration to a wire protocol.
///
/// The [`Debug`] implementation redacts every credential value to
/// `[REDACTED]`, so a formatted value is safe to log.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AuthScheme {
    /// Sends the value as an `Authorization: Bearer <token>` header.
    Bearer(String),
    /// Sends an endpoint-defined authentication header verbatim.
    Header {
        /// Header name required by the endpoint.
        name: String,
        /// Header value required by the endpoint.
        value: String,
    },
    /// Sends no authentication header.
    None,
}

impl fmt::Debug for AuthScheme {
    /// Prints the scheme name while redacting every credential value.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => write!(formatter, "Bearer({REDACTED})"),
            Self::Header { name, .. } => {
                write!(formatter, "Header {{ name: {name:?}, value: {REDACTED} }}")
            }
            Self::None => formatter.write_str("None"),
        }
    }
}

/// Transport details for one concrete LLM endpoint.
///
/// This configuration is independent of the endpoint's Anthropic Messages or
/// OpenAI Responses wire protocol. Query parameters and extra headers remain
/// ordered vectors so repeated keys and caller-specified order are preserved.
/// Adapters append their protocol path to `base_url`: `/v1/messages` for
/// Anthropic Messages and `/responses` for OpenAI Responses.
///
/// Although this type supports serde for application configuration, `auth`
/// contains credentials. Do not log or persist a serialized value unless the
/// destination is explicitly approved for secrets. The [`Debug`]
/// implementation, by contrast, is redaction-safe: [`EndpointConfig::auth`]
/// values and credential-looking [`EndpointConfig::extra_headers`] values are
/// replaced with `[REDACTED]`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointConfig {
    /// Base URL to which an adapter appends its protocol-specific path.
    pub base_url: String,
    /// Authentication scheme required by this endpoint.
    pub auth: AuthScheme,
    /// Query parameters appended to every request.
    ///
    /// Do not place secrets here: query strings are visible to proxies and
    /// appear in server logs. Transport error messages replace the entire
    /// query with `[REDACTED]` before surfacing a URL, but that redaction is
    /// a mitigation for error output, not a credential-protection mechanism.
    #[serde(default)]
    pub query_params: Vec<(String, String)>,
    /// Additional endpoint-specific headers appended to every request.
    ///
    /// Values of credential-looking headers (names containing fragments like
    /// `key`, `token`, `secret`, `auth`, `password`, or `credential`) are
    /// redacted in [`Debug`] output.
    #[serde(default)]
    pub extra_headers: Vec<(String, String)>,
}

impl fmt::Debug for EndpointConfig {
    /// Prints structural fields while redacting every credential-bearing value.
    ///
    /// `base_url` and `query_params` are shown verbatim; `auth` goes through
    /// the redacting [`AuthScheme`] [`Debug`]; `extra_headers` show header
    /// names with only non-sensitive values visible.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let extra_headers: Vec<(&str, &str)> = self
            .extra_headers
            .iter()
            .map(|(name, value)| {
                let value = if is_sensitive_header(name) {
                    REDACTED
                } else {
                    value.as_str()
                };
                (name.as_str(), value)
            })
            .collect();

        formatter
            .debug_struct("EndpointConfig")
            .field("base_url", &self.base_url)
            .field("auth", &self.auth)
            .field("query_params", &self.query_params)
            .field("extra_headers", &extra_headers)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthScheme, EndpointConfig};

    /// Verifies endpoint configuration can be persisted without losing order
    /// or provider-specific authentication details.
    fn assert_round_trip(config: EndpointConfig) {
        let encoded = serde_json::to_string(&config).expect("serialize endpoint config");
        let decoded: EndpointConfig =
            serde_json::from_str(&encoded).expect("deserialize endpoint config");

        assert_eq!(decoded, config);
    }

    #[test]
    fn anthropic_foundry_config_uses_bearer_auth_and_version_header() {
        let config = EndpointConfig {
            base_url: "https://anthropic.example.test".to_owned(),
            auth: AuthScheme::Bearer("anthropic-token".to_owned()),
            query_params: Vec::new(),
            extra_headers: vec![
                ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
                ("content-type".to_owned(), "application/json".to_owned()),
            ],
        };

        assert_eq!(
            config.auth,
            AuthScheme::Bearer("anthropic-token".to_owned())
        );
        assert!(config.query_params.is_empty());
        assert_eq!(
            config.extra_headers[0],
            ("anthropic-version".to_owned(), "2023-06-01".to_owned())
        );
        assert_round_trip(config);
    }

    #[test]
    fn openai_foundry_config_uses_api_key_header_and_api_version_query() {
        let config = EndpointConfig {
            base_url: "https://openai.example.test".to_owned(),
            auth: AuthScheme::Header {
                name: "api-key".to_owned(),
                value: "openai-token".to_owned(),
            },
            query_params: vec![("api-version".to_owned(), "2025-04-01-preview".to_owned())],
            extra_headers: Vec::new(),
        };

        assert_eq!(
            config.auth,
            AuthScheme::Header {
                name: "api-key".to_owned(),
                value: "openai-token".to_owned(),
            }
        );
        assert_eq!(
            config.query_params,
            vec![("api-version".to_owned(), "2025-04-01-preview".to_owned())]
        );
        assert_round_trip(config);
    }

    #[test]
    fn generic_header_and_no_auth_variants_have_stable_serde_shapes() {
        let header = AuthScheme::Header {
            name: "x-api-key".to_owned(),
            value: "direct-token".to_owned(),
        };
        let header_json = serde_json::to_value(&header).expect("serialize header auth");
        assert_eq!(header_json["type"], "header");
        assert_eq!(header_json["value"]["name"], "x-api-key");
        assert_eq!(
            serde_json::from_value::<AuthScheme>(header_json).expect("deserialize header auth"),
            header
        );

        let none_json = serde_json::to_value(&AuthScheme::None).expect("serialize no auth");
        assert_eq!(none_json["type"], "none");
        assert_eq!(
            serde_json::from_value::<AuthScheme>(none_json).expect("deserialize no auth"),
            AuthScheme::None
        );
    }

    #[test]
    fn auth_scheme_debug_redacts_every_credential_value() {
        let bearer = format!("{:?}", AuthScheme::Bearer("sk-ant-secret".to_owned()));
        assert!(
            !bearer.contains("sk-ant-secret"),
            "bearer token leaked: {bearer}"
        );
        assert_eq!(bearer, "Bearer([REDACTED])");

        let header = format!(
            "{:?}",
            AuthScheme::Header {
                name: "api-key".to_owned(),
                value: "sk-ant-secret".to_owned(),
            }
        );
        assert!(
            !header.contains("sk-ant-secret"),
            "header value leaked: {header}"
        );
        assert_eq!(header, "Header { name: \"api-key\", value: [REDACTED] }");

        assert_eq!(format!("{:?}", AuthScheme::None), "None");
    }

    #[test]
    fn endpoint_config_debug_redacts_auth_and_sensitive_extra_headers() {
        let config = EndpointConfig {
            base_url: "https://anthropic.example.test".to_owned(),
            auth: AuthScheme::Bearer("sk-ant-secret".to_owned()),
            query_params: vec![("api-version".to_owned(), "2025-04-01-preview".to_owned())],
            extra_headers: vec![
                ("api-key".to_owned(), "sk-ant-secret".to_owned()),
                ("x-api-key".to_owned(), "sk-ant-secret".to_owned()),
                ("authorization".to_owned(), "sk-ant-secret".to_owned()),
                ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
            ],
        };

        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("sk-ant-secret"),
            "secret leaked: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED]"),
            "missing placeholder: {rendered}"
        );
        // Structural fields stay visible for debuggability.
        assert!(rendered.contains("https://anthropic.example.test"));
        assert!(rendered.contains("api-version"));
        assert!(rendered.contains("2023-06-01"));
        // Header names survive even when their values are redacted.
        assert!(rendered.contains("api-key"));
    }

    #[test]
    fn endpoint_config_debug_preserves_serde_and_equality_behavior() {
        let config = EndpointConfig {
            base_url: "https://openai.example.test".to_owned(),
            auth: AuthScheme::Header {
                name: "api-key".to_owned(),
                value: "sk-ant-secret".to_owned(),
            },
            query_params: Vec::new(),
            extra_headers: Vec::new(),
        };

        assert_round_trip(config);
    }
}
