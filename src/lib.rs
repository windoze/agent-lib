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
//! - [`agent`] defines the data-only Agent identity and static configuration
//!   model used by future runtime layers.
//! - [`conversation`] adds externally supplied strong identities,
//!   Conversation-level configuration, immutable message envelopes, the
//!   canonical role/tool validator, an atomic closed-turn commit boundary, and
//!   a non-serializable [`conversation::PendingMessage`] freeze boundary inside
//!   the unique [`conversation::PendingTurn`] transaction. Pending turns support
//!   repeated and parallel tool round-trips while keeping partial Client data
//!   outside immutable history. [`conversation::CancelDisposition`] can discard,
//!   resume with explicit cancelled tool results, or atomically close and commit
//!   that transaction without touching previously committed turns. Committed
//!   turns live in structurally shared raw history, while the rebuildable
//!   [`conversation::ToolCallIndex`] accelerates framework/provider call lookup
//!   for only the current lineage and pending transaction. Versioned,
//!   Conversation-owned [`conversation::Boundary`] tokens name only complete
//!   Turn cuts; their position and stable anchor are revalidated against owner,
//!   structural version, lineage/fork range, and pending state before use.
//!   [`conversation::Conversation::revert_to`] moves a logical head backward or
//!   forward, rescopes derived lookup state, and retains every raw branch.
//!   [`conversation::Conversation::fork_at`] creates a child Conversation with
//!   its own owner/version metadata while sharing immutable prefix history and
//!   recording [`conversation::ForkOrigin`] provenance. Non-destructive
//!   [`conversation::Projection`] spans describe raw or compacted complete-Turn
//!   ranges; [`conversation::CheckedTurnRange`] stores stable Turn anchors so
//!   restored overlay data can be revalidated without trusting old Boundary
//!   versions. [`conversation::CompactionPlan`] and
//!   [`conversation::CompactionStep`] describe data-only overlay rewrites that
//!   [`conversation::Conversation::apply_compaction`] validates against the
//!   current owner/version/head before atomically replacing the projection
//!   without editing raw Turns. Runtime [`conversation::CompactionStrategy`]
//!   instances, [`conversation::CompactionStrategyResolver`] registries, and
//!   [`conversation::CompactionTrigger`] observers stay outside serde and
//!   produce only data plans, artifact drafts, or deferred boundary markers.
//!   [`conversation::Conversation::effective_view`] renders the head-clipped
//!   committed projection into Client-ready system/messages, while
//!   [`conversation::Conversation::pending_context`] exposes only frozen
//!   pending payloads through an explicit separate view.
//!   [`conversation::Conversation::snapshot`] exports versioned
//!   [`conversation::ConversationSnapshot`] data only at committed consistency
//!   points, excluding pending state, accumulators, derived indexes, and
//!   runtime strategy/trigger/client handles. [`conversation::ConversationRows`]
//!   decomposes the same snapshot facts into DB-neutral parent-tree rows with
//!   stable PK/FK fields, explicit sequences, and insert-only immutable
//!   Turn/message facts; row reassembly returns a snapshot that still must pass
//!   normal restore validation.
//!
//! Agent loops, tool registries, approval policy, and multi-agent orchestration
//! are still separate runtime layers. The [`agent`] module currently exposes
//! only serde-friendly static configuration and identity data, so those future
//! layers can persist references without storing live handles.
//!
//! # Conversation Core example
//!
//! Conversations receive every identity from the caller. A user payload enters
//! the unique pending transaction first; only a complete, tool-free final
//! assistant response can cross the closed-turn validator and become committed
//! history.
//!
//! ```
//! use agent_lib::{
//!     client::Response,
//!     conversation::{
//!         AssistantFinish, Conversation, ConversationConfig, ConversationId,
//!         MessageId, TurnId, TurnMeta,
//!     },
//!     model::{
//!         content::ContentBlock,
//!         message::{Message, Role},
//!         normalized::StopReason,
//!         usage::Usage,
//!     },
//! };
//! use serde_json::Map;
//!
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let conversation_id: ConversationId =
//!     "018f0d9c-7b6a-7c12-8f31-1234567890ab".parse()?;
//! let turn_id: TurnId =
//!     "018f0d9c-7b6a-7c12-8f31-1234567890ac".parse()?;
//! let user_message_id: MessageId =
//!     "018f0d9c-7b6a-7c12-8f31-1234567890ad".parse()?;
//! let assistant_message_id: MessageId =
//!     "018f0d9c-7b6a-7c12-8f31-1234567890ae".parse()?;
//!
//! let mut conversation = Conversation::new(
//!     conversation_id,
//!     ConversationConfig::new(Some("Answer briefly.".to_owned())),
//! );
//! conversation.begin_turn(
//!     turn_id,
//!     user_message_id,
//!     Message {
//!         role: Role::User,
//!         content: vec![ContentBlock::Text {
//!             text: "Explain the boundary.".to_owned(),
//!             extra: Map::new(),
//!         }],
//!     },
//! )?;
//!
//! conversation.start_assistant_response(Response {
//!     message: Message {
//!         role: Role::Assistant,
//!         content: vec![ContentBlock::Text {
//!             text: "Only complete turns are committed.".to_owned(),
//!             extra: Map::new(),
//!         }],
//!     },
//!     usage: Usage::default(),
//!     stop_reason: StopReason::normalize("end_turn"),
//!     extra: Map::new(),
//! })?;
//! assert_eq!(
//!     conversation.finish_assistant(assistant_message_id)?,
//!     AssistantFinish::ReadyToCommit
//! );
//! conversation.commit_pending(TurnMeta::default())?;
//!
//! let view = conversation.effective_view();
//! assert_eq!(view.system(), Some("Answer briefly."));
//! assert_eq!(view.messages().len(), 2);
//! # Ok(())
//! # }
//! # run().unwrap();
//! ```
//!
//! The repository also includes `cargo run --example conversation_core`, an
//! offline end-to-end example covering tool round-trips, cancellation followed
//! by continued feed, checked boundaries/forking, projection compaction, and
//! snapshot restore. It uses normalized local fixtures and does not access
//! network endpoints or runtime registries.
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
pub mod agent;
pub mod client;
pub mod conversation;
pub mod model;
pub mod stream;
