//! Runtime handle holder kept outside serialized Agent state.

/// Runtime handles rebuilt beside [`super::AgentState`] instead of serialized in it.
///
/// The generic parameters let host code carry concrete client, tool registry,
/// MCP session, approval responder, and task handle types without requiring
/// this crate to define those runtime traits in the state layer.
#[derive(Debug)]
pub struct AgentRuntimeHandles<
    ClientHandle,
    ToolRegistryHandle,
    McpSessionHandle = (),
    ApprovalResponderHandle = (),
    TaskHandle = (),
> {
    client: ClientHandle,
    tool_registry: ToolRegistryHandle,
    mcp_session: Option<McpSessionHandle>,
    approval_responder: Option<ApprovalResponderHandle>,
    task_handle: Option<TaskHandle>,
}

impl<ClientHandle, ToolRegistryHandle>
    AgentRuntimeHandles<ClientHandle, ToolRegistryHandle, (), (), ()>
{
    /// Creates a runtime holder with required client and tool-registry handles.
    #[must_use]
    pub fn new(client: ClientHandle, tool_registry: ToolRegistryHandle) -> Self {
        Self {
            client,
            tool_registry,
            mcp_session: None,
            approval_responder: None,
            task_handle: None,
        }
    }
}

impl<ClientHandle, ToolRegistryHandle, McpSessionHandle, ApprovalResponderHandle, TaskHandle>
    AgentRuntimeHandles<
        ClientHandle,
        ToolRegistryHandle,
        McpSessionHandle,
        ApprovalResponderHandle,
        TaskHandle,
    >
{
    /// Creates a runtime holder with every optional handle supplied explicitly.
    #[must_use]
    pub fn with_handles(
        client: ClientHandle,
        tool_registry: ToolRegistryHandle,
        mcp_session: Option<McpSessionHandle>,
        approval_responder: Option<ApprovalResponderHandle>,
        task_handle: Option<TaskHandle>,
    ) -> Self {
        Self {
            client,
            tool_registry,
            mcp_session,
            approval_responder,
            task_handle,
        }
    }

    /// Returns the live client handle.
    #[must_use]
    pub const fn client(&self) -> &ClientHandle {
        &self.client
    }

    /// Returns the live tool-registry handle.
    #[must_use]
    pub const fn tool_registry(&self) -> &ToolRegistryHandle {
        &self.tool_registry
    }

    /// Returns the optional MCP session handle.
    #[must_use]
    pub const fn mcp_session(&self) -> Option<&McpSessionHandle> {
        self.mcp_session.as_ref()
    }

    /// Returns the optional approval responder handle.
    #[must_use]
    pub const fn approval_responder(&self) -> Option<&ApprovalResponderHandle> {
        self.approval_responder.as_ref()
    }

    /// Returns the optional task handle.
    #[must_use]
    pub const fn task_handle(&self) -> Option<&TaskHandle> {
        self.task_handle.as_ref()
    }
}
