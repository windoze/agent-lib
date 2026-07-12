//! Provider-neutral Client and Conversation building blocks for LLM API access.
//!
//! `agent-lib` translates Anthropic Messages and OpenAI Responses wire formats
//! into one set of requests, complete responses, content blocks, token usage,
//! errors, and incremental events. Applications choose a concrete adapter at
//! the endpoint boundary and can use [`client::LlmClient`] everywhere else.
//!
//! # Architecture and boundaries
//!
//! - [`model`] contains complete-state messages, multimodal content, tools,
//!   normalized enum values, usage, and provider escape hatches.
//! - [`stream`] contains stable block identifiers, normalized deltas, and the
//!   one accumulator used to reconstruct a [`client::Response`].
//! - [`client`] defines endpoint configuration, structured capabilities,
//!   classified errors, requests, responses, and the dyn-safe client trait.
//! - [`adapter`] implements the Anthropic Messages and OpenAI Responses HTTP
//!   and SSE protocols.
//! - [`conversation`] adds externally supplied strong identities,
//!   Conversation-level configuration, immutable message envelopes, the
//!   canonical role/tool validator, an atomic closed-turn commit boundary, and
//!   a non-serializable [`conversation::PendingMessage`] freeze boundary inside
//!   the unique [`conversation::PendingTurn`] transaction. Pending turns support
//!   repeated and parallel tool round-trips while keeping partial Client data
//!   outside immutable history.
//!
//! Agent loops, tool registries, approval policy, and multi-agent orchestration
//! are deliberately outside this crate. Those layers should persist and replay
//! the normalized complete-state types instead of provider wire objects.
//!
//! # Complete-response example
//!
//! The endpoint configuration is independent of the normalized request. This
//! example uses Anthropic-compatible bearer authentication; deployments that
//! require `x-api-key`, `api-key`, or another header can use
//! [`client::AuthScheme::Header`].
//!
//! ```no_run
//! use agent_lib::{
//!     adapter::anthropic::AnthropicAdapter,
//!     client::{AuthScheme, ChatRequest, EndpointConfig, LlmClient},
//!     model::{
//!         content::ContentBlock,
//!         message::{Message, Role},
//!     },
//! };
//! use serde_json::Map;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = EndpointConfig {
//!     base_url: "https://llm.example.test".to_owned(),
//!     auth: AuthScheme::Bearer("secret-token".to_owned()),
//!     query_params: Vec::new(),
//!     extra_headers: vec![("anthropic-version".to_owned(), "2023-06-01".to_owned())],
//! };
//! let client: Box<dyn LlmClient> = Box::new(AnthropicAdapter::new(endpoint));
//! let request = ChatRequest {
//!     model: "claude-deployment".to_owned(),
//!     messages: vec![Message {
//!         role: Role::User,
//!         content: vec![ContentBlock::Text {
//!             text: "Say hello in one sentence.".to_owned(),
//!             extra: Map::new(),
//!         }],
//!     }],
//!     tools: Vec::new(),
//!     system: Some("Answer concisely.".to_owned()),
//!     max_tokens: 128,
//!     temperature: None,
//!     stream: false,
//!     provider_extras: None,
//! };
//!
//! let response = client.chat(request).await?;
//! println!("output tokens: {}", response.usage.output);
//! # Ok(())
//! # }
//! ```
//!
//! # Streaming discipline
//!
//! [`client::LlmClient::chat_stream`] emits [`stream::StreamEvent`] values.
//! Events for text, reasoning, and tool input all use stable block ids and the
//! same start/delta/stop lifecycle. Tool JSON deltas are deliberately raw and
//! must not be parsed until their complete boundary. Use
//! [`stream::accumulator::Accumulator`] while handling events interactively, or
//! [`stream::accumulator::collect`] when only the folded response is needed.
//!
//! # Forward compatibility
//!
//! Unknown provider response fields remain in `extra` maps, unrecognized enum
//! strings remain in [`model::normalized::Normalized::raw`], and request-only
//! dialect fields use provider-bound [`model::extras::ProviderExtras`]. This
//! keeps evidence available without leaking provider wire formats into callers.

#![warn(missing_docs)]

pub mod adapter;
pub mod client;
pub mod conversation;
pub mod model;
pub mod stream;
