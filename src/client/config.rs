//! Serializable endpoint transport configuration.

use serde::{Deserialize, Serialize};

/// Authentication applied when sending requests to an LLM endpoint.
///
/// [`AuthScheme::Header`] covers provider-specific names such as `api-key`
/// and `x-api-key` without coupling endpoint configuration to a wire protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
/// destination is explicitly approved for secrets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointConfig {
    /// Base URL to which an adapter appends its protocol-specific path.
    pub base_url: String,
    /// Authentication scheme required by this endpoint.
    pub auth: AuthScheme,
    /// Query parameters appended to every request.
    #[serde(default)]
    pub query_params: Vec<(String, String)>,
    /// Additional endpoint-specific headers appended to every request.
    #[serde(default)]
    pub extra_headers: Vec<(String, String)>,
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
}
