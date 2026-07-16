//! Runtime handle holder kept outside serialized external-agent state.

/// Live runtime handles rebuilt beside [`ExternalAgentState`] instead of
/// serialized in it.
///
/// This mirrors [`AgentRuntimeHandles`](crate::agent::AgentRuntimeHandles): the
/// generic parameters let host code carry the concrete runtime driver (CLI
/// process, SDK client, tmux pane, in-process teammate handle), an optional
/// interaction responder, an optional tool registry, and a session task set
/// without requiring this crate to name those runtime traits in the state layer.
/// None of these handles belong in [`ExternalAgentState`]'s serde shape; only
/// resumable facts are persisted (design §4.2/§4.3).
///
/// # Cleanup ownership (design §6.4)
///
/// The live session's process lifecycle is owned **here**, at the handle layer,
/// not by the machine. Cancelling an external agent is *never-resume*: the
/// driver abandons the continuation and the machine is not stepped again, so the
/// abandoned continuation can never emit a graceful
/// [`Shutdown`](crate::agent::ExternalSessionInput::Shutdown). The machine's
/// abandon step therefore only flags
/// [`ExternalAgentState::mark_cleanup_required`](crate::agent::ExternalAgentState::mark_cleanup_required);
/// the actual close is this holder's responsibility. By convention that close
/// happens one of two ways, so no continuation is required to run one more
/// effect:
///
/// - **Handle `Drop`.** This crate deliberately does not name the concrete
///   runtime handle types, so it cannot implement a meaningful blanket `Drop`;
///   instead each handle a host stores is expected to release its own resource
///   when this holder drops — a `tokio::process::Child` with `kill_on_drop`, an
///   aborting `JoinSet`/task set in `session_tasks`, a closing SDK client. This
///   is the same idiom as [`AgentRuntimeHandles`](crate::agent::AgentRuntimeHandles).
/// - **Container teardown.** Alternatively an owning session container/registry
///   sweeps orphaned sessions on teardown, closing any this holder still owns.
///
/// Either way the close is classified as an
/// [`ExternalSessionShutdown`](super::ExternalSessionShutdown) (graceful /
/// forced kill / failed) and recorded into the trace via
/// [`TraceHandle::record_external_shutdown`](crate::agent::TraceHandle::record_external_shutdown),
/// so a scheduler can decide whether the worktree is safe to reuse as clean
/// (design §6.4, §10).
///
/// [`ExternalAgentState`]: super::ExternalAgentState
#[derive(Debug)]
pub struct ExternalRuntimeHandles<
    Runtime,
    InteractionHandle = (),
    ToolRegistryHandle = (),
    SessionTasks = (),
> {
    runtime: Runtime,
    interaction: Option<InteractionHandle>,
    tool_registry: Option<ToolRegistryHandle>,
    session_tasks: SessionTasks,
}

impl<Runtime> ExternalRuntimeHandles<Runtime, (), (), ()> {
    /// Creates a runtime holder from the required runtime driver alone.
    ///
    /// The interaction responder and tool registry default to absent and the
    /// session task set to the unit placeholder; use
    /// [`with_handles`](Self::with_handles) to supply them.
    #[must_use]
    pub const fn new(runtime: Runtime) -> Self {
        Self {
            runtime,
            interaction: None,
            tool_registry: None,
            session_tasks: (),
        }
    }
}

impl<Runtime, InteractionHandle, ToolRegistryHandle, SessionTasks>
    ExternalRuntimeHandles<Runtime, InteractionHandle, ToolRegistryHandle, SessionTasks>
{
    /// Creates a runtime holder with every handle supplied explicitly.
    #[must_use]
    pub const fn with_handles(
        runtime: Runtime,
        interaction: Option<InteractionHandle>,
        tool_registry: Option<ToolRegistryHandle>,
        session_tasks: SessionTasks,
    ) -> Self {
        Self {
            runtime,
            interaction,
            tool_registry,
            session_tasks,
        }
    }

    /// Returns the live runtime driver handle.
    #[must_use]
    pub const fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Returns the optional interaction responder handle.
    #[must_use]
    pub const fn interaction(&self) -> Option<&InteractionHandle> {
        self.interaction.as_ref()
    }

    /// Returns the optional tool-registry handle.
    #[must_use]
    pub const fn tool_registry(&self) -> Option<&ToolRegistryHandle> {
        self.tool_registry.as_ref()
    }

    /// Returns the session task set handle.
    #[must_use]
    pub const fn session_tasks(&self) -> &SessionTasks {
        &self.session_tasks
    }

    /// Returns a mutable reference to the session task set handle.
    pub const fn session_tasks_mut(&mut self) -> &mut SessionTasks {
        &mut self.session_tasks
    }
}

#[cfg(test)]
mod tests {
    use super::ExternalRuntimeHandles;

    #[derive(Debug, PartialEq)]
    struct FakeRuntime(&'static str);

    #[test]
    fn external_runtime_handles_new_defaults_optional_handles_absent() {
        let handles = ExternalRuntimeHandles::new(FakeRuntime("claude-cli"));
        assert_eq!(handles.runtime(), &FakeRuntime("claude-cli"));
        assert!(handles.interaction().is_none());
        assert!(handles.tool_registry().is_none());
        assert_eq!(handles.session_tasks(), &());
    }

    #[test]
    fn external_runtime_handles_with_handles_exposes_every_slot() {
        let mut handles = ExternalRuntimeHandles::with_handles(
            FakeRuntime("codex-cli"),
            Some("interaction"),
            Some("tool-registry"),
            vec!["task-1"],
        );
        assert_eq!(handles.runtime(), &FakeRuntime("codex-cli"));
        assert_eq!(handles.interaction(), Some(&"interaction"));
        assert_eq!(handles.tool_registry(), Some(&"tool-registry"));
        assert_eq!(handles.session_tasks(), &vec!["task-1"]);

        handles.session_tasks_mut().push("task-2");
        assert_eq!(handles.session_tasks(), &vec!["task-1", "task-2"]);
    }
}
