//! Runtime tool registry and execution boundaries for Agent loops.
//!
//! These traits are live runtime handles, not serde data shapes. Static tool
//! declarations live in [`crate::agent::ToolSetRef`]; a loop uses a registry to
//! resolve those declarations to executable runtime behavior, and uses
//! [`ToolExecutionIds`] to obtain caller-supplied identities for every
//! Conversation bookkeeping boundary.

use crate::{
    agent::{StepId, ToolSetId, ToolSetRef},
    conversation::{MessageId, ToolCallId},
    model::{
        content::ContentBlock,
        tool::{Tool, ToolCall, ToolResponse, ToolStatus},
    },
};
use async_trait::async_trait;
use serde_json::Map;
use std::{collections::BTreeMap, fmt, sync::Arc};
use thiserror::Error;

/// Runtime executor for one provider-neutral model tool.
#[async_trait]
pub trait ToolExecutor: Send + Sync + fmt::Debug {
    /// Returns the declaration sent to a model when this executor is available.
    fn declaration(&self) -> &Tool;

    /// Executes one complete provider-neutral tool call.
    ///
    /// The framework-level [`ToolCallId`] is supplied by the Agent loop after
    /// the provider call id has been mapped through Conversation. Successful
    /// execution returns a complete [`ToolResponse`] carrying an explicit
    /// [`ToolStatus`].
    async fn execute(
        &self,
        call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError>;
}

/// Runtime registry used by an Agent loop to declare and execute tools.
#[async_trait]
pub trait ToolRegistry: Send + Sync + fmt::Debug {
    /// Returns the provider-neutral tool declarations currently available.
    fn declarations(&self) -> Vec<Tool>;

    /// Executes one complete tool call selected by the model.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRuntimeError`] if the tool name is unknown, the executor
    /// fails before producing a complete [`ToolResponse`], or the registry
    /// cannot resolve the call.
    async fn execute(
        &self,
        call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError>;
}

/// Runtime resolver for replacing an Agent's active tool registry.
///
/// The request data carries a [`ToolSetRef`], but executable callbacks remain
/// live runtime handles. A resolver is therefore responsible for mapping the
/// declared set to a registry at a turn boundary.
pub trait ToolRegistryResolver: Send + Sync + fmt::Debug {
    /// Resolves one static tool-set declaration into an executable registry.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRuntimeError::UnknownToolSet`] when the runtime has no
    /// registry for the requested set.
    fn resolve_tool_set(
        &self,
        tool_set: &ToolSetRef,
    ) -> Result<Arc<dyn ToolRegistry>, ToolRuntimeError>;
}

/// Caller-supplied identity source for Agent tool orchestration.
///
/// This trait deliberately does not generate ids. Implementations should draw
/// from host-provided queues, database rows, deterministic fixtures, or another
/// external allocation boundary.
pub trait ToolExecutionIds: Send + Sync + fmt::Debug {
    /// Returns the framework id for a provider tool-use block.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRuntimeError`] when no stable id is available.
    fn tool_call_id(&self, call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError>;

    /// Returns the message id used for the tool-result message.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRuntimeError`] when no stable id is available.
    fn tool_result_message_id(
        &self,
        call_id: ToolCallId,
        call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError>;

    /// Returns the next assistant message id after tool results have been added.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRuntimeError`] when no stable id is available.
    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError>;

    /// Returns the next Agent step id after tool results have been added.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRuntimeError`] when no stable id is available.
    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError>;
}

/// Registry that can advertise declarations but has no executable tools.
///
/// The default loop constructor uses this to preserve the static `AgentSpec`
/// request shape. Hosts that expect tool execution should pass a real registry
/// through [`crate::agent::DefaultAgentLoop::with_tool_registry`].
#[derive(Clone, Debug, Default)]
pub struct DeclaredOnlyToolRegistry {
    declarations: Vec<Tool>,
}

impl DeclaredOnlyToolRegistry {
    /// Creates a declared-only registry from static provider-neutral tools.
    #[must_use]
    pub fn new(declarations: Vec<Tool>) -> Self {
        Self { declarations }
    }
}

/// Resolver that creates declared-only registries from supplied declarations.
///
/// This is appropriate for loops that only need to advertise tools. Hosts that
/// need executable callbacks should supply a stricter resolver.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeclaredOnlyToolRegistryResolver;

impl ToolRegistryResolver for DeclaredOnlyToolRegistryResolver {
    fn resolve_tool_set(
        &self,
        tool_set: &ToolSetRef,
    ) -> Result<Arc<dyn ToolRegistry>, ToolRuntimeError> {
        Ok(Arc::new(DeclaredOnlyToolRegistry::new(
            tool_set.tools().to_vec(),
        )))
    }
}

/// Resolver backed by a fixed registry catalog keyed by `ToolSetId`.
#[derive(Clone, Debug, Default)]
pub struct StaticToolRegistryResolver {
    registries: BTreeMap<ToolSetId, Arc<dyn ToolRegistry>>,
}

impl StaticToolRegistryResolver {
    /// Creates an empty registry catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a runtime registry for one tool-set identity.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRuntimeError::InvalidRegistry`] if the id is already
    /// present.
    pub fn insert(
        &mut self,
        tool_set_id: ToolSetId,
        registry: Arc<dyn ToolRegistry>,
    ) -> Result<(), ToolRuntimeError> {
        if self.registries.insert(tool_set_id, registry).is_some() {
            return Err(ToolRuntimeError::InvalidRegistry {
                message: format!("duplicate registry for tool set {tool_set_id}"),
            });
        }
        Ok(())
    }

    /// Creates a catalog with one known registry.
    #[must_use]
    pub fn single(tool_set_id: ToolSetId, registry: Arc<dyn ToolRegistry>) -> Self {
        let mut registries = BTreeMap::new();
        registries.insert(tool_set_id, registry);
        Self { registries }
    }
}

impl ToolRegistryResolver for StaticToolRegistryResolver {
    fn resolve_tool_set(
        &self,
        tool_set: &ToolSetRef,
    ) -> Result<Arc<dyn ToolRegistry>, ToolRuntimeError> {
        self.registries
            .get(&tool_set.id())
            .cloned()
            .ok_or(ToolRuntimeError::UnknownToolSet { id: tool_set.id() })
    }
}

#[async_trait]
impl ToolRegistry for DeclaredOnlyToolRegistry {
    fn declarations(&self) -> Vec<Tool> {
        self.declarations.clone()
    }

    async fn execute(
        &self,
        _call_id: ToolCallId,
        call: ToolCall,
    ) -> Result<ToolResponse, ToolRuntimeError> {
        Err(ToolRuntimeError::UnknownTool { name: call.name })
    }
}

/// Identity provider that never supplies ids.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoToolExecutionIds;

impl ToolExecutionIds for NoToolExecutionIds {
    fn tool_call_id(&self, call: &ToolCall) -> Result<ToolCallId, ToolRuntimeError> {
        Err(ToolRuntimeError::IdUnavailable {
            purpose: format!("tool call `{}`", call.id),
        })
    }

    fn tool_result_message_id(
        &self,
        _call_id: ToolCallId,
        call: &ToolCall,
    ) -> Result<MessageId, ToolRuntimeError> {
        Err(ToolRuntimeError::IdUnavailable {
            purpose: format!("tool result for `{}`", call.id),
        })
    }

    fn next_assistant_message_id(&self) -> Result<MessageId, ToolRuntimeError> {
        Err(ToolRuntimeError::IdUnavailable {
            purpose: "assistant continuation message".to_owned(),
        })
    }

    fn next_step_id(&self) -> Result<StepId, ToolRuntimeError> {
        Err(ToolRuntimeError::IdUnavailable {
            purpose: "assistant continuation step".to_owned(),
        })
    }
}

/// Classified runtime failure from tool orchestration.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ToolRuntimeError {
    /// The registry has no executable tool with this name.
    #[error("unknown tool `{name}`")]
    UnknownTool {
        /// Tool name selected by the model.
        name: String,
    },
    /// The runtime has no registry for a requested tool-set identity.
    #[error("unknown tool set `{id}`")]
    UnknownToolSet {
        /// Tool-set identity selected by a reconfiguration request.
        id: ToolSetId,
    },
    /// The host did not provide a stable identity required by the loop.
    #[error("missing externally supplied id for {purpose}")]
    IdUnavailable {
        /// Stable description of the missing identity.
        purpose: String,
    },
    /// The executor failed before returning a complete `ToolResponse`.
    #[error("tool `{tool_name}` failed: {message}")]
    ExecutionFailed {
        /// Tool name selected by the model.
        tool_name: String,
        /// Stable diagnostic text.
        message: String,
    },
    /// The registry itself rejected construction or lookup data.
    #[error("invalid tool registry: {message}")]
    InvalidRegistry {
        /// Stable diagnostic text.
        message: String,
    },
}

impl ToolRuntimeError {
    /// Converts this failure into a model-visible failed tool result.
    #[must_use]
    pub fn to_tool_response(&self, provider_call_id: impl Into<String>) -> ToolResponse {
        ToolResponse {
            tool_call_id: provider_call_id.into(),
            content: vec![ContentBlock::Text {
                text: self.to_string(),
                extra: Map::new(),
            }],
            status: ToolStatus::Error,
            extra: Map::new(),
        }
    }
}
