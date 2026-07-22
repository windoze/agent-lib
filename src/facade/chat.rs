//! One-shot Chat facade over a raw [`Conversation`] drive.
//!
//! [`Chat`] is the shareable configuration and client-assembly entry point of
//! the facade. It bundles a concrete [`LlmClient`] with a [`ModelConfig`] and an
//! optional system prompt, and exposes [`Chat::ask`] / [`Chat::ask_full`] for
//! stateless one-shot generations.
//!
//! Milestone 1 drives [`Conversation`] directly rather than through the Agent
//! machine (see `docs/facade-api.md` §5.3): each call begins a throwaway
//! transaction (`begin_turn`), renders a [`ChatRequest`] from the effective view
//! plus the pending user message, calls [`LlmClient::chat`], folds the response
//! back (`start_assistant_response` → `finish_assistant`), and commits only a
//! tool-free final assistant response (`commit_pending`).
//!
//! The Chat facade never executes tools: a response carrying a tool-use block is
//! a hard [`FacadeError::UnexpectedToolUse`] rather than a loop step. Callers who
//! need tools should use the Agent facade (Milestone 2).
//!
//! The stateful [`ChatSession`] reuses a single [`Conversation`] across turns so
//! history accumulates. Its [`stream`](ChatSession::stream) entry point folds an
//! incremental [`crate::stream::accumulator::Accumulator`] into the same
//! [`Response`](crate::client::Response) the non-streaming path produces. Both
//! the one-shot [`Chat`] and the stateful [`ChatSession`] share the same private
//! `drive_turn` drive.

use std::sync::Arc;

use crate::adapter::anthropic::AnthropicAdapter;
use crate::adapter::openai_chat::OpenAiChatAdapter;
use crate::adapter::openai_resp::OpenAiRespAdapter;
use crate::client::{ChatRequest, LlmClient};
use crate::conversation::{
    AssistantFinish, CancelDisposition, Conversation, ConversationConfig, ConversationSnapshot,
    TurnMeta,
};
use crate::facade::config::{
    ModelConfig, ProviderConfig, ensure_non_blank_model, ensure_provider_extras_match_provider,
};
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::facade::run::{IntoUserMessage, Reply, RunOutput};
use crate::model::extras::ProviderExtras;

mod stream;

pub use stream::RunStream;

/// A shareable Chat configuration bound to one concrete [`LlmClient`].
///
/// A `Chat` is cheap to clone (the client is shared behind an [`Arc`]) and holds
/// no per-conversation state, so the same value can drive any number of
/// independent one-shot [`ask`](Chat::ask) calls concurrently.
///
/// # Example
///
/// ```no_run
/// # async fn demo() -> Result<(), agent_lib::facade::FacadeError> {
/// use agent_lib::facade::{Chat, ProviderConfig};
///
/// let chat = Chat::builder()
///     .provider(ProviderConfig::openai_from_env()?)
///     .model("gpt-5.5")
///     .system("Answer concisely.")
///     .build()?;
///
/// let reply = chat.ask("What is a provider-neutral client?").await?;
/// println!("{}", reply.text());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Chat {
    client: Arc<dyn LlmClient>,
    model: ModelConfig,
    system: Option<String>,
    ids: FacadeIds,
}

impl std::fmt::Debug for Chat {
    /// Prints the model and system prompt while treating the client as opaque.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Chat")
            .field("client", &"<dyn LlmClient>")
            .field("model", &self.model)
            .field("system", &self.system)
            .finish()
    }
}

impl Chat {
    /// Starts a fluent [`ChatBuilder`].
    #[must_use]
    pub fn builder() -> ChatBuilder {
        ChatBuilder::new()
    }

    /// Returns the shared client this Chat drives.
    #[must_use]
    pub fn client(&self) -> &Arc<dyn LlmClient> {
        &self.client
    }

    /// Returns the model configuration applied to every request.
    #[must_use]
    pub const fn model(&self) -> &ModelConfig {
        &self.model
    }

    /// Returns the system prompt applied to every one-shot conversation, if any.
    #[must_use]
    pub fn system(&self) -> Option<&str> {
        self.system.as_deref()
    }

    /// Runs one stateless generation and returns the minimal [`Reply`].
    ///
    /// This is a convenience wrapper over [`ask_full`](Chat::ask_full); see that
    /// method for the exact drive and error semantics.
    ///
    /// # Errors
    ///
    /// Returns any [`FacadeError`] produced by [`ask_full`](Chat::ask_full),
    /// including [`FacadeError::UnexpectedToolUse`] when the model asks to call a
    /// tool.
    pub async fn ask(&self, input: impl IntoUserMessage) -> Result<Reply, FacadeError> {
        Ok(self.ask_full(input).await?.reply)
    }

    /// Runs one stateless generation and returns the full [`RunOutput`].
    ///
    /// Each call builds a fresh throwaway [`Conversation`] seeded only with this
    /// Chat's system prompt, so no history is retained between calls. The drive
    /// follows `docs/facade-api.md` §5.3.
    ///
    /// # Errors
    ///
    /// - [`FacadeError::Client`] if the underlying [`LlmClient::chat`] call fails.
    /// - [`FacadeError::UnexpectedToolUse`] if the model returns a tool-use
    ///   block (the Chat facade never executes tools).
    /// - [`FacadeError::Conversation`] if folding the response through the
    ///   Conversation transaction is rejected.
    ///
    /// On any error the throwaway transaction is discarded, so a shared `Chat`
    /// remains usable.
    pub async fn ask_full(&self, input: impl IntoUserMessage) -> Result<RunOutput, FacadeError> {
        let mut conversation = Conversation::new(
            self.ids.conversation_id(),
            ConversationConfig::new(self.system.clone()),
        );
        drive_turn(
            &mut conversation,
            &*self.client,
            &self.model,
            &self.ids,
            input,
        )
        .await
    }

    /// Starts a fluent [`ChatSessionBuilder`] for a stateful multi-turn session.
    ///
    /// The returned builder inherits this Chat's client, model, identity source,
    /// and (unless overridden) system prompt. Unlike [`ask`](Chat::ask), a
    /// [`ChatSession`] retains history across turns.
    #[must_use]
    pub fn session(&self) -> ChatSessionBuilder {
        ChatSessionBuilder::new(self.clone())
    }
}

/// Drives one complete Chat turn against a caller-owned [`Conversation`].
///
/// This is the shared `docs/facade-api.md` §5.3 drive used by the one-shot
/// [`Chat`] and (from Milestone 1-4) the stateful `ChatSession`. On any error
/// after the transaction is opened, the pending turn is discarded so the
/// Conversation returns to its last committed, consistent point.
async fn drive_turn(
    conversation: &mut Conversation,
    client: &dyn LlmClient,
    model: &ModelConfig,
    ids: &FacadeIds,
    input: impl IntoUserMessage,
) -> Result<RunOutput, FacadeError> {
    let user_payload = input.into_user_message();
    conversation.begin_turn(ids.turn_id(), ids.message_id(), user_payload)?;

    match drive_pending(conversation, client, model, ids).await {
        Ok(output) => Ok(output),
        Err(error) => {
            // Roll back the uncommitted turn so the Conversation stays at a
            // committed, consistent point (a no-op when nothing is pending).
            let _ = conversation.cancel_pending(CancelDisposition::DiscardTurn);
            Err(error)
        }
    }
}

/// Completes the currently pending turn: request, fold, and commit.
async fn drive_pending(
    conversation: &mut Conversation,
    client: &dyn LlmClient,
    model: &ModelConfig,
    ids: &FacadeIds,
) -> Result<RunOutput, FacadeError> {
    let request = build_request(conversation, model, false);
    let response = client.chat(request).await?;

    conversation.start_assistant_response(response.clone())?;
    match conversation.finish_assistant(ids.message_id())? {
        AssistantFinish::ReadyToCommit => {}
        AssistantFinish::RequiresToolCallMappings => return Err(FacadeError::UnexpectedToolUse),
    }
    conversation.commit_pending(TurnMeta::default())?;

    Ok(RunOutput::from(response))
}

/// Renders a [`ChatRequest`] from committed history plus the frozen pending user
/// message.
///
/// The Chat facade never advertises tools, so `tools` is always empty. `stream`
/// selects the non-streaming ([`LlmClient::chat`]) or streaming
/// ([`LlmClient::chat_stream`]) wire path.
fn build_request(conversation: &Conversation, model: &ModelConfig, stream: bool) -> ChatRequest {
    let (system, mut messages) = conversation.effective_view().into_parts();
    if let Some(pending) = conversation.pending_context() {
        messages.extend(pending.into_messages());
    }

    let mut request = ChatRequest {
        model: String::new(),
        messages,
        tools: Vec::new(),
        system,
        max_tokens: 0,
        temperature: None,
        stream,
        provider_extras: None,
    };
    model.apply_to_request(&mut request);
    request
}

/// A fluent builder for [`Chat`].
///
/// Set either an explicit [`client`](ChatBuilder::client) (handy for offline
/// tests) or a [`provider`](ChatBuilder::provider) from which a concrete adapter
/// client is constructed, then a model and optional generation parameters.
#[derive(Clone, Default)]
pub struct ChatBuilder {
    provider: Option<ProviderConfig>,
    client: Option<Arc<dyn LlmClient>>,
    model: Option<String>,
    system: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    provider_extras: Option<ProviderExtras>,
    ids: Option<FacadeIds>,
}

impl ChatBuilder {
    /// Creates an empty builder.
    #[must_use]
    fn new() -> Self {
        Self::default()
    }

    /// Sets the provider used to construct the client when none is injected.
    ///
    /// Ignored when an explicit [`client`](ChatBuilder::client) is also set.
    #[must_use]
    pub fn provider(mut self, provider: ProviderConfig) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Injects a concrete client, bypassing provider-based construction.
    ///
    /// This is the recommended path for offline tests: a scripted fake client
    /// can be supplied without touching the network.
    #[must_use]
    pub fn client(mut self, client: Arc<dyn LlmClient>) -> Self {
        self.client = Some(client);
        self
    }

    /// Sets the model or deployment identifier (required).
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Sets an optional system prompt applied to every generation.
    #[must_use]
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Sets the maximum number of output tokens.
    ///
    /// A value of `0` is treated as "leave at the default" (see
    /// [`ModelConfig::max_tokens`]).
    #[must_use]
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Sets the sampling temperature.
    #[must_use]
    pub fn temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Sets provider-specific request fields for every generated request.
    ///
    /// When this builder also has a [`provider`](Self::provider), the extras'
    /// [`ProviderId`](crate::model::extras::ProviderId) must match that provider.
    /// Builders that use only an injected [`client`](Self::client) cannot infer a
    /// provider id and pass the extras through to the injected client unchanged.
    #[must_use]
    pub fn provider_extras(mut self, provider_extras: ProviderExtras) -> Self {
        self.provider_extras = Some(provider_extras);
        self
    }

    /// Overrides the built-in identity source (mainly for deterministic tests).
    #[must_use]
    pub fn ids(mut self, ids: FacadeIds) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Finalizes the builder into a [`Chat`].
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Config`] when no model was set, or when neither an
    /// explicit client nor a provider was supplied.
    pub fn build(self) -> Result<Chat, FacadeError> {
        let model_name = self.model.ok_or_else(|| {
            FacadeError::Config("chat configuration is missing a `model`".to_owned())
        })?;
        let model_name = ensure_non_blank_model("chat", model_name)?;

        let mut model = ModelConfig::new(model_name);
        if let Some(max_tokens) = self.max_tokens {
            model = model.max_tokens(max_tokens);
        }
        if let Some(temperature) = self.temperature {
            model = model.temperature(temperature)?;
        }
        if let Some(provider_extras) = &self.provider_extras {
            ensure_provider_extras_match_provider(
                "chat",
                self.provider.as_ref().map(ProviderConfig::provider),
                provider_extras,
            )?;
        }
        if let Some(provider_extras) = self.provider_extras {
            model = model.provider_extras(provider_extras);
        }

        let client = match (self.client, self.provider) {
            (Some(client), _) => client,
            (None, Some(provider)) => client_for_provider(provider),
            (None, None) => {
                return Err(FacadeError::Config(
                    "chat configuration needs either a `client` or a `provider`".to_owned(),
                ));
            }
        };

        Ok(Chat {
            client,
            model,
            system: self.system,
            ids: self.ids.unwrap_or_default(),
        })
    }
}

/// Builds a concrete adapter client for a [`ProviderConfig`]'s wire protocol.
pub(crate) fn client_for_provider(provider: ProviderConfig) -> Arc<dyn LlmClient> {
    use crate::model::extras::ProviderId;

    let (endpoint, provider_id) = provider.into_parts();
    match provider_id {
        ProviderId::Anthropic => Arc::new(AnthropicAdapter::new(endpoint)),
        ProviderId::OpenAiResp => Arc::new(OpenAiRespAdapter::new(endpoint)),
        ProviderId::OpenAiChat => Arc::new(OpenAiChatAdapter::new(endpoint)),
    }
}

/// A stateful, multi-turn Chat session backed by one live [`Conversation`].
///
/// Unlike the one-shot [`Chat::ask`], a `ChatSession` reuses a single
/// [`Conversation`] and identity source across turns, so each
/// [`send`](ChatSession::send) appends to the committed history and subsequent
/// requests replay the full context (`docs/facade-api.md` §5.1–§5.3).
///
/// A session is created from a [`Chat`] via [`Chat::session`], which inherits the
/// client, model, identity source, and system prompt:
///
/// ```no_run
/// # async fn demo() -> Result<(), agent_lib::facade::FacadeError> {
/// use agent_lib::facade::{Chat, ProviderConfig};
///
/// let chat = Chat::builder()
///     .provider(ProviderConfig::openai_from_env()?)
///     .model("gpt-5.5")
///     .system("Answer concisely.")
///     .build()?;
///
/// let mut session = chat.session().build()?;
/// let _first = session.send("Explain ownership.").await?;
/// let _second = session.send("Give an example.").await?;
/// # Ok(())
/// # }
/// ```
///
/// The session state is a plain [`Conversation`]; it never holds the provider
/// configuration or credentials, so a [`snapshot`](ChatSession::snapshot) is safe
/// to persist and a [`restore`](ChatSession::restore) re-injects the client from a
/// caller-supplied [`Chat`].
pub struct ChatSession {
    conversation: Conversation,
    client: Arc<dyn LlmClient>,
    model: ModelConfig,
    ids: FacadeIds,
}

impl std::fmt::Debug for ChatSession {
    /// Prints the conversation and model while treating the client as opaque.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatSession")
            .field("conversation", &self.conversation)
            .field("client", &"<dyn LlmClient>")
            .field("model", &self.model)
            .finish()
    }
}

impl ChatSession {
    /// Runs one turn against the retained history and returns the [`Reply`].
    ///
    /// This is a convenience wrapper over [`send_full`](ChatSession::send_full);
    /// see that method for the exact drive and error semantics.
    ///
    /// # Errors
    ///
    /// Returns any [`FacadeError`] produced by
    /// [`send_full`](ChatSession::send_full), including
    /// [`FacadeError::UnexpectedToolUse`] when the model asks to call a tool.
    pub async fn send(&mut self, input: impl IntoUserMessage) -> Result<Reply, FacadeError> {
        Ok(self.send_full(input).await?.reply)
    }

    /// Runs one turn against the retained history and returns the [`RunOutput`].
    ///
    /// The turn continues this session's [`Conversation`]: the request replays the
    /// committed history plus the new user message, and a tool-free assistant
    /// response is committed so the next turn sees it. The drive follows
    /// `docs/facade-api.md` §5.3.
    ///
    /// # Errors
    ///
    /// - [`FacadeError::Client`] if the underlying [`LlmClient::chat`] call fails.
    /// - [`FacadeError::UnexpectedToolUse`] if the model returns a tool-use
    ///   block (the Chat facade never executes tools).
    /// - [`FacadeError::Conversation`] if folding the response through the
    ///   Conversation transaction is rejected.
    ///
    /// On any error the in-flight turn is discarded, so the session returns to its
    /// last committed, consistent point and remains usable.
    pub async fn send_full(
        &mut self,
        input: impl IntoUserMessage,
    ) -> Result<RunOutput, FacadeError> {
        drive_turn(
            &mut self.conversation,
            &*self.client,
            &self.model,
            &self.ids,
            input,
        )
        .await
    }

    /// Runs one turn as an incremental [`RunStream`] over the retained history.
    ///
    /// The returned stream forwards each normalized
    /// [`RunEvent::TextDelta`](crate::facade::RunEvent::TextDelta) and the
    /// underlying [`RunEvent::RawStream`](crate::facade::RunEvent::RawStream)
    /// escape hatch as they arrive, then yields exactly one terminal
    /// [`RunEvent::Done`](crate::facade::RunEvent::Done) carrying the complete
    /// [`RunOutput`]. Internally the incremental events are folded with a
    /// [`stream::accumulator::Accumulator`](crate::stream::accumulator::Accumulator)
    /// into the same [`Response`](crate::client::Response) the non-streaming
    /// [`send_full`](ChatSession::send_full) would produce, so the terminal
    /// `RunOutput` (text, usage, response) matches turn for turn.
    ///
    /// Only when the terminal `Done` is reached is the assistant response
    /// committed to this session's [`Conversation`]; until then no history is
    /// mutated beyond opening the pending turn. Dropping the stream before it
    /// completes discards the in-flight turn, leaving the session at its last
    /// committed point.
    ///
    /// # Errors
    ///
    /// The `await` itself returns:
    ///
    /// - [`FacadeError::Client`] if [`LlmClient::chat_stream`] fails before the
    ///   response headers arrive.
    /// - [`FacadeError::Conversation`] if opening the pending turn is rejected.
    ///
    /// Failures observed while streaming (transport errors, malformed events) and
    /// a [`FacadeError::UnexpectedToolUse`] for a tool-use stream (the Chat facade
    /// never executes tools) are surfaced as an `Err` item yielded by the stream.
    /// On any such failure the in-flight turn is discarded, so the session returns
    /// to its last committed, consistent point and remains usable.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn demo(mut session: agent_lib::facade::ChatSession)
    /// #     -> Result<(), agent_lib::facade::FacadeError> {
    /// use agent_lib::facade::RunEvent;
    ///
    /// let mut stream = session.stream("Write a short poem.").await?;
    /// while let Some(event) = stream.next().await.transpose()? {
    ///     match event {
    ///         RunEvent::TextDelta(text) => print!("{text}"),
    ///         RunEvent::Done(output) => eprintln!("usage={:?}", output.usage),
    ///         _ => {}
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn stream(
        &mut self,
        input: impl IntoUserMessage,
    ) -> Result<RunStream<'_>, FacadeError> {
        let turn_id = self.ids.turn_id();
        let user_message_id = self.ids.message_id();
        self.conversation
            .begin_turn(turn_id, user_message_id, input.into_user_message())?;

        let request = build_request(&self.conversation, &self.model, true);
        let inner = match self.client.chat_stream(request).await {
            Ok(inner) => inner,
            Err(error) => {
                // Roll back the just-opened turn so the session stays consistent.
                let _ = self
                    .conversation
                    .cancel_pending(CancelDisposition::DiscardTurn);
                return Err(FacadeError::from(error));
            }
        };

        Ok(RunStream::new(
            &mut self.conversation,
            inner,
            self.ids.clone(),
        ))
    }

    /// Returns the live [`Conversation`] backing this session.
    ///
    /// Useful for inspecting accumulated history, for example via
    /// [`Conversation::effective_view`].
    #[must_use]
    pub const fn conversation(&self) -> &Conversation {
        &self.conversation
    }

    /// Captures a data-only [`ConversationSnapshot`] of the committed history.
    ///
    /// The snapshot carries only conversation facts — never the client, provider
    /// configuration, or credentials — so it is safe to persist and later
    /// [`restore`](ChatSession::restore) against a fresh [`Chat`].
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Conversation`] wrapping a
    /// [`SnapshotError::PendingTurn`](crate::conversation::SnapshotError::PendingTurn)
    /// if an uncommitted turn is in flight. In normal use each
    /// [`send`](ChatSession::send) commits before returning, so the session rests
    /// at a snapshot-able consistency point.
    pub fn snapshot(&self) -> Result<ConversationSnapshot, FacadeError> {
        Ok(self.conversation.snapshot()?)
    }

    /// Rebuilds a session from a [`ConversationSnapshot`], re-injecting a client.
    ///
    /// The snapshot restores the committed history; the supplied [`Chat`] provides
    /// the client and model to continue the session. Pending turns are
    /// intentionally absent from snapshots and are never restored, so the rebuilt
    /// session begins at a committed consistency point.
    ///
    /// A fresh identity source is derived with
    /// [`FacadeIds::continuing_after`], seeded past every id in the restored
    /// history. This matters because a [`ConversationSnapshot`] is data-only and
    /// carries no runtime counter: reusing the [`Chat`]'s counter (which restarts
    /// at `1` on a new process) would otherwise re-mint ids that already exist in
    /// the restored history and be rejected as duplicates.
    ///
    /// # Errors
    ///
    /// Returns [`FacadeError::Conversation`] wrapping a
    /// [`RestoreError`](crate::conversation::RestoreError) when the snapshot is
    /// malformed or its schema version is unsupported.
    pub fn restore(snapshot: ConversationSnapshot, chat: Chat) -> Result<Self, FacadeError> {
        let conversation = Conversation::restore(snapshot)?;
        let ids = FacadeIds::continuing_after(&conversation);
        Ok(Self {
            conversation,
            client: chat.client,
            model: chat.model,
            ids,
        })
    }
}

/// A fluent builder for a [`ChatSession`], created via [`Chat::session`].
///
/// The builder inherits the originating [`Chat`]'s client, model, identity
/// source, and system prompt. Use [`system`](ChatSessionBuilder::system) to
/// override the inherited system prompt for this session only, or
/// [`clear_system`](ChatSessionBuilder::clear_system) to explicitly start without
/// one.
#[derive(Clone)]
pub struct ChatSessionBuilder {
    chat: Chat,
    system: Option<String>,
    system_overridden: bool,
}

impl std::fmt::Debug for ChatSessionBuilder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatSessionBuilder")
            .field("chat", &self.chat)
            .field("system", &self.system)
            .field("system_overridden", &self.system_overridden)
            .finish()
    }
}

impl ChatSessionBuilder {
    /// Creates a builder that inherits configuration from `chat`.
    fn new(chat: Chat) -> Self {
        Self {
            chat,
            system: None,
            system_overridden: false,
        }
    }

    /// Overrides the system prompt for this session.
    ///
    /// When unset, the session inherits the originating [`Chat`]'s system prompt.
    #[must_use]
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self.system_overridden = true;
        self
    }

    /// Clears the inherited system prompt for this session.
    #[must_use]
    pub fn clear_system(mut self) -> Self {
        self.system = None;
        self.system_overridden = true;
        self
    }

    /// Finalizes the builder into a fresh, empty [`ChatSession`].
    ///
    /// The new session starts a fresh [`Conversation`] seeded with the effective
    /// system prompt (the override, if any, else the inherited one).
    ///
    /// # Errors
    ///
    /// Currently infallible, but returns a [`Result`] so future configuration
    /// validation can surface a [`FacadeError`] without a breaking change.
    pub fn build(self) -> Result<ChatSession, FacadeError> {
        let system = if self.system_overridden {
            self.system
        } else {
            self.chat.system.clone()
        };
        let conversation = Conversation::new(
            self.chat.ids.conversation_id(),
            ConversationConfig::new(system),
        );
        Ok(ChatSession {
            conversation,
            client: self.chat.client,
            model: self.chat.model,
            ids: self.chat.ids,
        })
    }
}

#[cfg(test)]
mod tests;
