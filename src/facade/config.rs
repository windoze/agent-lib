//! Ergonomic provider and model configuration wrappers.
//!
//! [`ProviderConfig`] wraps a [`crate::client::EndpointConfig`] together with the
//! [`ProviderId`] that names the wire protocol, and offers environment-based and
//! builder constructors so callers do not have to assemble headers and query
//! parameters by hand. [`ModelConfig`] wraps the common per-request model
//! parameters (`model`, `max_tokens`, `temperature`, provider extras) and can be
//! projected into the lower-layer [`crate::agent::ModelRef`] or copied onto a
//! [`crate::client::ChatRequest`].
//!
//! Credentials live inside [`ProviderConfig`]. Its [`std::fmt::Debug`] output is
//! redacted and it is never serialized into snapshots, so a value can be logged
//! without leaking an API key. Do not persist a `ProviderConfig`; reconstruct it
//! from the environment or a secret store instead.

use std::env;
use std::fmt;
use std::num::NonZeroU32;

use crate::agent::ModelRef;
use crate::client::{AuthScheme, ChatRequest, EndpointConfig};
use crate::facade::error::FacadeError;
use crate::model::extras::{ProviderExtras, ProviderId};

/// Default Anthropic Messages base URL used when no override is supplied.
const ANTHROPIC_DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// Default `anthropic-version` header value.
const ANTHROPIC_DEFAULT_VERSION: &str = "2023-06-01";
/// Default OpenAI Responses `api-version` query value.
const OPENAI_DEFAULT_API_VERSION: &str = "2025-04-01-preview";
/// Fallback maximum output tokens when a [`ModelConfig`] does not set one.
const DEFAULT_MAX_TOKENS: u32 = 1024;

/// A provider endpoint plus the wire protocol it speaks.
///
/// Construct one from the environment ([`ProviderConfig::anthropic_from_env`],
/// [`ProviderConfig::openai_from_env`], [`ProviderConfig::openai_chat_from_env`]),
/// from a fluent builder ([`ProviderConfig::anthropic`],
/// [`ProviderConfig::openai`], [`ProviderConfig::openai_chat`]), or directly
/// from an already-built [`EndpointConfig`] ([`ProviderConfig::custom`]).
///
/// # Credential handling
///
/// This type may hold an API key inside its [`EndpointConfig::auth`]. Its
/// [`Debug`] implementation is redacted and it deliberately does not implement
/// [`serde::Serialize`]; do not log or persist it as a snapshot (see
/// `docs/facade-api.md` §4.1).
#[derive(Clone)]
pub struct ProviderConfig {
    endpoint: EndpointConfig,
    provider: ProviderId,
}

impl ProviderConfig {
    /// Wraps an already-built [`EndpointConfig`] with its wire protocol.
    #[must_use]
    pub fn custom(endpoint: EndpointConfig, provider: ProviderId) -> Self {
        Self { endpoint, provider }
    }

    /// Returns the wrapped endpoint transport configuration.
    #[must_use]
    pub const fn endpoint(&self) -> &EndpointConfig {
        &self.endpoint
    }

    /// Returns the wire protocol this endpoint speaks.
    #[must_use]
    pub const fn provider(&self) -> ProviderId {
        self.provider
    }

    /// Consumes the wrapper, returning its endpoint and provider parts.
    #[must_use]
    pub fn into_parts(self) -> (EndpointConfig, ProviderId) {
        (self.endpoint, self.provider)
    }

    /// Builds an Anthropic Messages provider from environment variables.
    ///
    /// Reads `ANTHROPIC_BASE_URL` (defaulting to `https://api.anthropic.com`),
    /// the required bearer token `ANTHROPIC_AUTH_TOKEN`, and the optional
    /// `ANTHROPIC_VERSION` (defaulting to `2023-06-01`, sent as the
    /// `anthropic-version` header).
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Config`] when the required auth token is missing
    /// or blank. The error message names the variable but never its value.
    pub fn anthropic_from_env() -> Result<Self, FacadeError> {
        let base_url = optional_env("ANTHROPIC_BASE_URL", ANTHROPIC_DEFAULT_BASE_URL);
        let token = required_env("ANTHROPIC_AUTH_TOKEN")?;
        let version = optional_env("ANTHROPIC_VERSION", ANTHROPIC_DEFAULT_VERSION);
        Ok(Self::custom(
            anthropic_endpoint(base_url, token, version),
            ProviderId::Anthropic,
        ))
    }

    /// Builds an OpenAI Responses provider from environment variables.
    ///
    /// Reads the required `OPENAI_BASE_URL` and `OPENAI_API_KEY` (sent as the
    /// `api-key` header) plus the optional `OPENAI_API_VERSION` (defaulting to
    /// `2025-04-01-preview`, sent as the `api-version` query parameter).
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Config`] when a required variable is missing or
    /// blank. The error message names the variable but never its value.
    pub fn openai_from_env() -> Result<Self, FacadeError> {
        let base_url = required_env("OPENAI_BASE_URL")?;
        let api_key = required_env("OPENAI_API_KEY")?;
        let api_version = optional_env("OPENAI_API_VERSION", OPENAI_DEFAULT_API_VERSION);
        Ok(Self::custom(
            openai_endpoint(base_url, api_key, api_version),
            ProviderId::OpenAiResp,
        ))
    }

    /// Builds an OpenAI Chat/Completions provider from environment variables.
    ///
    /// This is the direct-access classic `POST /v1/chat/completions` path: it
    /// reads the required `OPENAI_CHAT_BASE_URL` and the optional
    /// `OPENAI_CHAT_API_KEY` (sent as a `Bearer` token). Unlike
    /// [`ProviderConfig::openai_from_env`] it uses Bearer auth, not the
    /// Azure-style `api-key` header + `api-version` query.
    ///
    /// When `OPENAI_CHAT_API_KEY` is absent or blank the endpoint is built with
    /// [`AuthScheme::None`], matching unauthenticated OpenAI-compatible servers
    /// such as vLLM. Point `OPENAI_CHAT_BASE_URL` at the target server, for
    /// example `https://api.deepseek.com` for DeepSeek or `http://host:port/v1`
    /// for vLLM; dialect-specific request fields (DeepSeek thinking mode,
    /// reasoning effort) travel through [`ProviderExtras`].
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Config`] when `OPENAI_CHAT_BASE_URL` is missing or
    /// blank. The error message names the variable but never its value.
    pub fn openai_chat_from_env() -> Result<Self, FacadeError> {
        let base_url = required_env("OPENAI_CHAT_BASE_URL")?;
        let auth = match optional_owned_env("OPENAI_CHAT_API_KEY") {
            Some(api_key) => AuthScheme::Bearer(api_key),
            None => AuthScheme::None,
        };
        Ok(Self::custom(
            openai_chat_endpoint(base_url, auth),
            ProviderId::OpenAiChat,
        ))
    }

    /// Starts a fluent builder for an Anthropic Messages provider.
    #[must_use]
    pub fn anthropic() -> ProviderConfigBuilder {
        ProviderConfigBuilder::new(ProviderId::Anthropic)
    }

    /// Starts a fluent builder for an OpenAI Responses provider.
    #[must_use]
    pub fn openai() -> ProviderConfigBuilder {
        ProviderConfigBuilder::new(ProviderId::OpenAiResp)
    }

    /// Starts a fluent builder for an OpenAI Chat/Completions provider.
    ///
    /// Like [`ProviderConfig::openai_chat_from_env`] this targets the classic
    /// `POST /v1/chat/completions` endpoint with direct Bearer auth (no
    /// Azure-style `api-key` header or `api-version` query). The fluent builder
    /// always requires an [`api_key`](ProviderConfigBuilder::api_key); for an
    /// unauthenticated endpoint (vLLM) use [`ProviderConfig::openai_chat_from_env`]
    /// or [`ProviderConfig::custom`] with [`AuthScheme::None`].
    #[must_use]
    pub fn openai_chat() -> ProviderConfigBuilder {
        ProviderConfigBuilder::new(ProviderId::OpenAiChat)
    }
}

impl fmt::Debug for ProviderConfig {
    /// Prints structural fields while redacting every credential-bearing value.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConfig")
            .field("provider", &self.provider)
            .field("base_url", &self.endpoint.base_url)
            .field("auth", &RedactedAuth(&self.endpoint.auth))
            .field("query_params", &RedactedPairs(&self.endpoint.query_params))
            .field(
                "extra_headers",
                &RedactedPairs(&self.endpoint.extra_headers),
            )
            .finish()
    }
}

/// Debug helper that reveals an auth scheme's shape without its secret value.
struct RedactedAuth<'a>(&'a AuthScheme);

impl fmt::Debug for RedactedAuth<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            AuthScheme::Bearer(_) => formatter.write_str("Bearer(<redacted>)"),
            AuthScheme::Header { name, .. } => {
                write!(formatter, "Header {{ name: {name:?}, value: <redacted> }}")
            }
            AuthScheme::None => formatter.write_str("None"),
        }
    }
}

/// Debug helper that lists key/value pair names while redacting their values.
///
/// Query parameters and extra headers can carry credentials (for example an
/// `x-api-key` header), so only their keys are shown.
struct RedactedPairs<'a>(&'a [(String, String)]);

impl fmt::Debug for RedactedPairs<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut list = formatter.debug_list();
        for (key, _value) in self.0 {
            list.entry(&format_args!("({key:?}, <redacted>)"));
        }
        list.finish()
    }
}

/// A fluent builder for [`ProviderConfig`].
///
/// Obtain one from [`ProviderConfig::anthropic`] or [`ProviderConfig::openai`],
/// set the endpoint fields, then call [`ProviderConfigBuilder::build`].
#[derive(Clone)]
pub struct ProviderConfigBuilder {
    provider: ProviderId,
    base_url: Option<String>,
    api_key: Option<String>,
    api_version: Option<String>,
}

impl ProviderConfigBuilder {
    /// Creates a builder targeting `provider`.
    fn new(provider: ProviderId) -> Self {
        Self {
            provider,
            base_url: None,
            api_key: None,
            api_version: None,
        }
    }

    /// Sets the endpoint base URL (required).
    #[must_use]
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Sets the API key or bearer token (required).
    #[must_use]
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    /// Sets the protocol version override (optional; a default is used when
    /// omitted).
    ///
    /// For Anthropic this becomes the `anthropic-version` header; for OpenAI
    /// Responses it becomes the `api-version` query parameter. Chat/Completions
    /// has no version parameter, so this field is ignored for that provider.
    #[must_use]
    pub fn api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = Some(api_version.into());
        self
    }

    /// Finalizes the builder into a [`ProviderConfig`].
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Config`] when `base_url` or `api_key` was never
    /// set (or set to a blank value).
    pub fn build(self) -> Result<ProviderConfig, FacadeError> {
        let base_url = require_field("base_url", self.base_url)?;
        let api_key = require_field("api_key", self.api_key)?;
        let endpoint = match self.provider {
            ProviderId::Anthropic => {
                let version = self
                    .api_version
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| ANTHROPIC_DEFAULT_VERSION.to_owned());
                anthropic_endpoint(base_url, api_key, version)
            }
            ProviderId::OpenAiResp => {
                let version = self
                    .api_version
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| OPENAI_DEFAULT_API_VERSION.to_owned());
                openai_endpoint(base_url, api_key, version)
            }
            // Classic chat/completions direct access: Bearer auth, no Azure-style
            // api-version header/query. The env constructor (`openai_chat_from_env`)
            // handles the vLLM no-auth case; this generic builder path always
            // carries a resolved api_key.
            ProviderId::OpenAiChat => openai_chat_endpoint(base_url, AuthScheme::Bearer(api_key)),
        };
        Ok(ProviderConfig::custom(endpoint, self.provider))
    }
}

/// Builds the Anthropic Messages endpoint transport from resolved parts.
fn anthropic_endpoint(base_url: String, token: String, version: String) -> EndpointConfig {
    EndpointConfig {
        base_url,
        auth: AuthScheme::Bearer(token),
        query_params: Vec::new(),
        extra_headers: vec![("anthropic-version".to_owned(), version)],
    }
}

/// Builds the OpenAI Responses endpoint transport from resolved parts.
fn openai_endpoint(base_url: String, api_key: String, api_version: String) -> EndpointConfig {
    EndpointConfig {
        base_url,
        auth: AuthScheme::Header {
            name: "api-key".to_owned(),
            value: api_key,
        },
        query_params: vec![("api-version".to_owned(), api_version)],
        extra_headers: Vec::new(),
    }
}

/// Builds the OpenAI Chat/Completions endpoint transport from resolved parts.
///
/// Chat/completions uses direct auth (Bearer for OpenAI-compatible/DeepSeek/vLLM
/// with a key, `None` for unauthenticated vLLM), not the Azure-style `api-key`
/// header + `api-version` query of [`openai_endpoint`]. The caller picks the
/// [`AuthScheme`]; query params and extra headers stay empty.
fn openai_chat_endpoint(base_url: String, auth: AuthScheme) -> EndpointConfig {
    EndpointConfig {
        base_url,
        auth,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    }
}

/// Reads a required, non-blank environment variable.
fn required_env(name: &str) -> Result<String, FacadeError> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) | Err(_) => Err(FacadeError::Config(format!(
            "required environment variable {name} is not set"
        ))),
    }
}

/// Reads a non-blank optional environment variable or returns `default`.
fn optional_env(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// Reads a non-blank optional environment variable, returning `None` when unset
/// or blank. Used for credentials that may legitimately be absent (for example
/// `OPENAI_CHAT_API_KEY` on a no-auth vLLM endpoint).
fn optional_owned_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

/// Validates a required builder field is present and non-blank.
fn require_field(name: &str, value: Option<String>) -> Result<String, FacadeError> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(FacadeError::Config(format!(
            "provider configuration is missing required field `{name}`"
        ))),
    }
}

/// Common per-request model parameters shared by the Chat and Agent facades.
///
/// A `ModelConfig` carries only data that can be copied into a
/// [`crate::agent::ModelRef`] or a [`crate::client::ChatRequest`]; it never
/// holds a client, endpoint, or credential. When `max_tokens` is left unset it
/// defaults to `1024`.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelConfig {
    model: String,
    max_tokens: NonZeroU32,
    temperature: Option<f32>,
    provider_extras: Option<ProviderExtras>,
}

impl ModelConfig {
    /// Creates a model configuration for `model` with default parameters.
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            max_tokens: NonZeroU32::new(DEFAULT_MAX_TOKENS)
                .expect("default max tokens is non-zero"),
            temperature: None,
            provider_extras: None,
        }
    }

    /// Sets the maximum number of output tokens.
    ///
    /// A `max_tokens` of `0` is meaningless for a generation request and is
    /// treated as "leave at the default"; any non-zero value is used verbatim.
    #[must_use]
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        if let Some(value) = NonZeroU32::new(max_tokens) {
            self.max_tokens = value;
        }
        self
    }

    /// Sets the sampling temperature.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Config`] when `temperature` is `NaN` or infinite.
    pub fn temperature(mut self, temperature: f32) -> Result<Self, FacadeError> {
        ensure_finite_temperature("model", temperature)?;
        self.temperature = Some(temperature);
        Ok(self)
    }

    /// Sets provider-specific request extras, bound to their target provider.
    #[must_use]
    pub fn provider_extras(mut self, provider_extras: ProviderExtras) -> Self {
        self.provider_extras = Some(provider_extras);
        self
    }

    /// Returns the configured model or deployment identifier.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the configured maximum output token count.
    #[must_use]
    pub const fn max_tokens_value(&self) -> NonZeroU32 {
        self.max_tokens
    }

    /// Returns the configured sampling temperature, if any.
    #[must_use]
    pub const fn temperature_value(&self) -> Option<f32> {
        self.temperature
    }

    /// Returns the configured provider extras, if any.
    #[must_use]
    pub const fn provider_extras_value(&self) -> Option<&ProviderExtras> {
        self.provider_extras.as_ref()
    }

    /// Projects this configuration into the agent-layer [`ModelRef`].
    #[must_use]
    pub fn to_model_ref(&self) -> ModelRef {
        ModelRef::new(
            self.model.clone(),
            self.max_tokens,
            self.temperature,
            self.provider_extras.clone(),
        )
    }

    /// Copies the shared model parameters onto a [`ChatRequest`].
    ///
    /// Overwrites `model`, `max_tokens`, `temperature`, and `provider_extras`,
    /// leaving `messages`, `tools`, `system`, and `stream` untouched.
    pub fn apply_to_request(&self, request: &mut ChatRequest) {
        request.model = self.model.clone();
        request.max_tokens = self.max_tokens.get();
        request.temperature = self.temperature;
        request.provider_extras = self.provider_extras.clone();
    }
}

/// Verifies a model or deployment identifier is not blank.
pub(crate) fn ensure_non_blank_model(builder: &str, model: String) -> Result<String, FacadeError> {
    if model.trim().is_empty() {
        return Err(FacadeError::Config(format!(
            "{builder} configuration has a blank `model`"
        )));
    }
    Ok(model)
}

/// Verifies a sampling temperature can be serialized as a finite JSON number.
pub(crate) fn ensure_finite_temperature(
    builder: &str,
    temperature: f32,
) -> Result<f32, FacadeError> {
    if temperature.is_finite() {
        return Ok(temperature);
    }

    Err(FacadeError::Config(format!(
        "{builder} configuration has a non-finite `temperature`"
    )))
}

/// Verifies builder-level provider extras target the configured provider.
///
/// A builder that only receives an injected client has no reliable provider id:
/// [`crate::client::Capability`] describes features, not wire protocol. In that
/// escape-hatch case this helper leaves the extras untouched so the injected
/// client can decide how to handle them.
pub(crate) fn ensure_provider_extras_match_provider(
    builder: &str,
    provider: Option<ProviderId>,
    provider_extras: &ProviderExtras,
) -> Result<(), FacadeError> {
    let Some(provider) = provider else {
        return Ok(());
    };
    if provider_extras.provider == provider {
        return Ok(());
    }

    Err(FacadeError::Config(format!(
        "{builder} provider_extras target {:?}, but provider is {:?}",
        provider_extras.provider, provider
    )))
}

#[cfg(test)]
mod tests;
