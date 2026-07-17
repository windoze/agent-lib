//! Unit tests for [`ProviderConfig`] and [`ModelConfig`].
//!
//! Every test is offline: environment-based constructors are exercised with a
//! process-wide env guard that sets and restores variables around one test, and
//! no real credentials are used. Because these tests mutate process environment
//! variables, they are serialized through a shared mutex.

use super::{ModelConfig, ProviderConfig};
use crate::client::{AuthScheme, ChatRequest, EndpointConfig};
use crate::model::extras::{ProviderExtras, ProviderId};
use serde_json::{Map, json};
use std::num::NonZeroU32;
use std::sync::Mutex;

/// Serializes env-mutating tests so concurrent cases cannot observe each
/// other's variables.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Removes a set of environment variables, restoring their prior values on drop.
struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    /// Captures and clears the given variables for the duration of a test.
    fn clearing(names: &[&str]) -> Self {
        let saved = names
            .iter()
            .map(|name| ((*name).to_owned(), std::env::var(name).ok()))
            .collect();
        for name in names {
            unsafe {
                std::env::remove_var(name);
            }
        }
        Self { saved }
    }

    /// Sets a variable within the guarded scope.
    fn set(&self, name: &str, value: &str) {
        unsafe {
            std::env::set_var(name, value);
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}

#[test]
fn custom_preserves_endpoint_and_provider() {
    let endpoint = EndpointConfig {
        base_url: "https://endpoint.example.test".to_owned(),
        auth: AuthScheme::Bearer("secret-token".to_owned()),
        query_params: vec![("api-version".to_owned(), "2025-01-01".to_owned())],
        extra_headers: vec![("x-trace".to_owned(), "on".to_owned())],
    };

    let config = ProviderConfig::custom(endpoint.clone(), ProviderId::Anthropic);

    assert_eq!(config.provider(), ProviderId::Anthropic);
    assert_eq!(config.endpoint(), &endpoint);

    let (parts_endpoint, parts_provider) = config.into_parts();
    assert_eq!(parts_endpoint, endpoint);
    assert_eq!(parts_provider, ProviderId::Anthropic);
}

#[test]
fn anthropic_builder_sets_bearer_auth_and_version_header() {
    let config = ProviderConfig::anthropic()
        .base_url("https://anthropic.example.test")
        .api_key("anthropic-token")
        .api_version("2024-02-15")
        .build()
        .expect("anthropic builder should succeed with required fields");

    assert_eq!(config.provider(), ProviderId::Anthropic);
    let endpoint = config.endpoint();
    assert_eq!(endpoint.base_url, "https://anthropic.example.test");
    assert_eq!(
        endpoint.auth,
        AuthScheme::Bearer("anthropic-token".to_owned())
    );
    assert!(endpoint.query_params.is_empty());
    assert_eq!(
        endpoint.extra_headers,
        vec![("anthropic-version".to_owned(), "2024-02-15".to_owned())]
    );
}

#[test]
fn anthropic_builder_defaults_version_when_omitted() {
    let config = ProviderConfig::anthropic()
        .base_url("https://anthropic.example.test")
        .api_key("anthropic-token")
        .build()
        .expect("anthropic builder should succeed without an explicit version");

    assert_eq!(
        config.endpoint().extra_headers,
        vec![("anthropic-version".to_owned(), "2023-06-01".to_owned())]
    );
}

#[test]
fn openai_builder_sets_api_key_header_and_api_version_query() {
    let config = ProviderConfig::openai()
        .base_url("https://openai.example.test")
        .api_key("openai-token")
        .api_version("2025-04-01-preview")
        .build()
        .expect("openai builder should succeed with required fields");

    assert_eq!(config.provider(), ProviderId::OpenAiResp);
    let endpoint = config.endpoint();
    assert_eq!(endpoint.base_url, "https://openai.example.test");
    assert_eq!(
        endpoint.auth,
        AuthScheme::Header {
            name: "api-key".to_owned(),
            value: "openai-token".to_owned(),
        }
    );
    assert_eq!(
        endpoint.query_params,
        vec![("api-version".to_owned(), "2025-04-01-preview".to_owned())]
    );
    assert!(endpoint.extra_headers.is_empty());
}

#[test]
fn builder_reports_config_error_for_missing_required_fields() {
    let missing_base_url = ProviderConfig::openai().api_key("token").build();
    assert!(matches!(
        missing_base_url,
        Err(crate::facade::FacadeError::Config(_))
    ));

    let missing_api_key = ProviderConfig::anthropic()
        .base_url("https://anthropic.example.test")
        .build();
    assert!(matches!(
        missing_api_key,
        Err(crate::facade::FacadeError::Config(_))
    ));
}

#[test]
fn anthropic_from_env_reads_variables_and_defaults() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let guard = EnvGuard::clearing(&[
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_VERSION",
    ]);
    guard.set("ANTHROPIC_AUTH_TOKEN", "env-anthropic-token");

    let config = ProviderConfig::anthropic_from_env().expect("anthropic env config");

    assert_eq!(config.provider(), ProviderId::Anthropic);
    let endpoint = config.endpoint();
    assert_eq!(endpoint.base_url, "https://api.anthropic.com");
    assert_eq!(
        endpoint.auth,
        AuthScheme::Bearer("env-anthropic-token".to_owned())
    );
    assert_eq!(
        endpoint.extra_headers,
        vec![("anthropic-version".to_owned(), "2023-06-01".to_owned())]
    );
}

#[test]
fn anthropic_from_env_errors_when_token_missing() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _guard = EnvGuard::clearing(&[
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_VERSION",
    ]);

    let result = ProviderConfig::anthropic_from_env();

    assert!(matches!(result, Err(crate::facade::FacadeError::Config(_))));
}

#[test]
fn openai_from_env_errors_when_required_variable_missing() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let guard = EnvGuard::clearing(&["OPENAI_BASE_URL", "OPENAI_API_KEY", "OPENAI_API_VERSION"]);
    // Provide only the base URL; the api key remains unset.
    guard.set("OPENAI_BASE_URL", "https://openai.example.test");

    let result = ProviderConfig::openai_from_env();

    assert!(matches!(result, Err(crate::facade::FacadeError::Config(_))));
}

#[test]
fn debug_redacts_bearer_token_and_header_value() {
    let config = ProviderConfig::anthropic()
        .base_url("https://anthropic.example.test")
        .api_key("super-secret-anthropic-key")
        .build()
        .expect("anthropic builder");

    let rendered = format!("{config:?}");
    assert!(!rendered.contains("super-secret-anthropic-key"));
    assert!(rendered.contains("<redacted>"));
    assert!(rendered.contains("https://anthropic.example.test"));
    assert!(rendered.contains("anthropic-version"));
}

#[test]
fn debug_redacts_header_auth_value() {
    let config = ProviderConfig::openai()
        .base_url("https://openai.example.test")
        .api_key("super-secret-openai-key")
        .build()
        .expect("openai builder");

    let rendered = format!("{config:?}");
    assert!(!rendered.contains("super-secret-openai-key"));
    assert!(rendered.contains("<redacted>"));
}

#[test]
fn model_config_builder_and_defaults() {
    let default_config = ModelConfig::new("gpt-5.5");
    assert_eq!(default_config.model(), "gpt-5.5");
    assert_eq!(
        default_config.max_tokens_value(),
        NonZeroU32::new(1024).unwrap()
    );
    assert_eq!(default_config.temperature_value(), None);

    let config = ModelConfig::new("gpt-5.5")
        .max_tokens(2048)
        .temperature(0.2);
    assert_eq!(config.max_tokens_value(), NonZeroU32::new(2048).unwrap());
    assert_eq!(config.temperature_value(), Some(0.2));
}

#[test]
fn model_config_max_tokens_zero_keeps_default() {
    let config = ModelConfig::new("gpt-5.5").max_tokens(0);
    assert_eq!(config.max_tokens_value(), NonZeroU32::new(1024).unwrap());
}

#[test]
fn to_model_ref_maps_every_field() {
    let extras = ProviderExtras {
        provider: ProviderId::Anthropic,
        fields: Map::from_iter([("top_k".to_owned(), json!(25))]),
    };
    let config = ModelConfig::new("claude-test")
        .max_tokens(4096)
        .temperature(0.7)
        .provider_extras(extras.clone());

    let model_ref = config.to_model_ref();

    assert_eq!(model_ref.model(), "claude-test");
    assert_eq!(model_ref.max_tokens(), NonZeroU32::new(4096).unwrap());
    assert_eq!(model_ref.temperature(), Some(0.7));
    assert_eq!(model_ref.provider_extras(), Some(&extras));
}

#[test]
fn apply_to_request_overwrites_only_shared_fields() {
    let extras = ProviderExtras {
        provider: ProviderId::OpenAiResp,
        fields: Map::from_iter([("reasoning".to_owned(), json!({ "effort": "high" }))]),
    };
    let config = ModelConfig::new("gpt-5.5")
        .max_tokens(1500)
        .temperature(0.4)
        .provider_extras(extras.clone());

    let mut request = ChatRequest {
        model: "placeholder".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
        system: Some("stay concise".to_owned()),
        max_tokens: 1,
        temperature: Some(1.0),
        stream: true,
        provider_extras: None,
    };

    config.apply_to_request(&mut request);

    assert_eq!(request.model, "gpt-5.5");
    assert_eq!(request.max_tokens, 1500);
    assert_eq!(request.temperature, Some(0.4));
    assert_eq!(request.provider_extras, Some(extras));
    // Untouched fields survive.
    assert_eq!(request.system.as_deref(), Some("stay concise"));
    assert!(request.stream);
}
