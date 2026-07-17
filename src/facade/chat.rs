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
//! need tools should use the Agent facade (Milestone 2). The stateful
//! [`ChatSession`](crate::facade) and streaming entry points land in later
//! Milestone 1 tasks.

use std::sync::Arc;

use crate::adapter::anthropic::AnthropicAdapter;
use crate::adapter::openai_resp::OpenAiRespAdapter;
use crate::client::{ChatRequest, LlmClient};
use crate::conversation::{
    AssistantFinish, CancelDisposition, Conversation, ConversationConfig, TurnMeta,
};
use crate::facade::config::{ModelConfig, ProviderConfig};
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::facade::run::{IntoUserMessage, Reply, RunOutput};

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
    let request = build_request(conversation, model);
    let response = client.chat(request).await?;

    conversation.start_assistant_response(response.clone())?;
    match conversation.finish_assistant(ids.message_id())? {
        AssistantFinish::ReadyToCommit => {}
        AssistantFinish::RequiresToolCallMappings => return Err(FacadeError::UnexpectedToolUse),
    }
    conversation.commit_pending(TurnMeta::default())?;

    Ok(RunOutput::from(response))
}

/// Renders a non-streaming [`ChatRequest`] from committed history plus the
/// frozen pending user message.
///
/// The Chat facade never advertises tools, so `tools` is always empty.
fn build_request(conversation: &Conversation, model: &ModelConfig) -> ChatRequest {
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
        stream: false,
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

        let mut model = ModelConfig::new(model_name);
        if let Some(max_tokens) = self.max_tokens {
            model = model.max_tokens(max_tokens);
        }
        if let Some(temperature) = self.temperature {
            model = model.temperature(temperature);
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
fn client_for_provider(provider: ProviderConfig) -> Arc<dyn LlmClient> {
    use crate::model::extras::ProviderId;

    let (endpoint, provider_id) = provider.into_parts();
    match provider_id {
        ProviderId::Anthropic => Arc::new(AnthropicAdapter::new(endpoint)),
        ProviderId::OpenAiResp => Arc::new(OpenAiRespAdapter::new(endpoint)),
    }
}

#[cfg(test)]
mod tests;
