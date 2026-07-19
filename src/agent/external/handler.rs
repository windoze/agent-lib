//! Registry-backed [`ExternalSessionHandler`](crate::agent::ExternalSessionHandler).
//!
//! Driving a managed external agent through the sans-io
//! [`ExternalAgentMachine`](super::ExternalAgentMachine) requires *some*
//! [`ExternalSessionHandler`] to advance the real runtime: the machine reifies
//! each round-trip as a `NeedExternalSession` requirement and hands it to the
//! driver, which owns the live IO. Every host that ran a real CLI agent had to
//! hand-assemble the same "last mile" — take a live
//! [`ExternalRuntimeAdapter`](super::ExternalRuntimeAdapter), wrap it in an
//! [`ExternalSessionRegistry`], and fold each `get_or_start → advance` round-trip
//! into an [`ExternalSessionResult`]. This module ships that composition **once**
//! as production library code so a host injects it directly rather than copying
//! it.
//!
//! [`RegistryExternalSessionHandler`] holds no machine state. Each
//! [`fulfill`](ExternalSessionHandler::fulfill):
//!
//! 1. resolves the live handle through the registry —
//!    [`get_or_start`](ExternalSessionRegistry::get_or_start) starts the session
//!    on the first [`Start`](super::ExternalSessionInput::Start) and reattaches to
//!    the same live handle on every follow-up turn (reusing the registry's
//!    capability-gated resume path when a session outlives its live handle);
//! 2. advances it exactly one [`RuntimeDecisionPoint`](super::RuntimeDecisionPoint)
//!    (never running the session to completion in one blocking call);
//! 3. folds the outcome into a family-aligned
//!    [`RequirementResult::ExternalSession`] via the milestone-5 `From`
//!    conversion, so a `get_or_start` **or** an `advance` failure both surface as
//!    an [`ExternalSessionResult::Failed`] rather than the wrong requirement
//!    family.
//!
//! The handler is runtime-agnostic: any adapter behind the registry works. The
//! feature-gated `default_external_session_handler`
//! (`crate::facade::default_external_session_handler`) is the ergonomic
//! constructor that probes a real CLI and wires the matching live adapter behind
//! one of these handlers.
//!
//! # Cleanup
//!
//! The machine never emits a `Shutdown` effect — force-closing a live session is
//! a handle-layer concern (design §16). The facade drive path force-closes
//! automatically: when a managed drive ends cancelled or failed it calls
//! [`cleanup_agent`](ExternalSessionHandler::cleanup_agent), which this handler
//! forwards to [`ExternalSessionRegistry::cleanup_agent`], so a host that does
//! nothing extra leaks no subprocess (M3-2). A *completed* session is left live
//! for reuse; a host force-closes it (or sweeps ahead of teardown) through the
//! [`registry`](RegistryExternalSessionHandler::registry) accessor with
//! [`cleanup_agent`](ExternalSessionRegistry::cleanup_agent) /
//! [`cleanup`](ExternalSessionRegistry::cleanup) so no orphaned runtime process
//! is left behind.

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::drive::ExternalSessionHandler;
use crate::agent::requirement::RequirementResult;
use crate::agent::{AgentId, RunContext};

use super::{
    ExternalEventSink, ExternalSessionRegistry, ExternalSessionRequest, ExternalSessionResult,
    ExternalSessionShutdown, RuntimeDecisionPoint,
};

/// A production-shaped [`ExternalSessionHandler`] that advances managed external
/// sessions through an [`ExternalSessionRegistry`].
///
/// It holds no machine state: the whole session lives in the registry, and each
/// [`fulfill`](ExternalSessionHandler::fulfill) resolves the live handle, advances
/// it one decision point, and folds the outcome into an
/// [`ExternalSessionResult`] (see the type-level docs below). Construct one over a
/// registry with [`new`](Self::new), or with a live
/// [`ExternalEventSink`] via [`with_sink`](Self::with_sink) to tail observations
/// as the runtime streams them.
pub struct RegistryExternalSessionHandler {
    registry: Arc<ExternalSessionRegistry>,
    sink: Option<Arc<dyn ExternalEventSink>>,
}

impl std::fmt::Debug for RegistryExternalSessionHandler {
    /// Renders the registry's runtime kind and whether a live sink is attached,
    /// treating the sink as opaque so no host attachment leaks into diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistryExternalSessionHandler")
            .field("runtime", &self.registry.kind())
            .field("live_sessions", &self.registry.live_len())
            .field("has_sink", &self.sink.is_some())
            .finish()
    }
}

impl RegistryExternalSessionHandler {
    /// Builds a handler over `registry` with no live sink.
    ///
    /// Observations reached on the way to each decision point are still returned
    /// in the [`ExternalSessionResult`] for the machine to convert into
    /// notifications; a sink is only needed to *also* tail them live.
    #[must_use]
    pub fn new(registry: Arc<ExternalSessionRegistry>) -> Self {
        Self {
            registry,
            sink: None,
        }
    }

    /// Builds a handler over `registry` that forwards live observations to `sink`.
    ///
    /// The sink is handed to the adapter when a session is started or resumed, so
    /// a host can tail streamed events as they arrive; it is ignored when
    /// reattaching to an existing live handle whose sink was wired at creation.
    #[must_use]
    pub fn with_sink(
        registry: Arc<ExternalSessionRegistry>,
        sink: Arc<dyn ExternalEventSink>,
    ) -> Self {
        Self {
            registry,
            sink: Some(sink),
        }
    }

    /// Returns the registry that owns this handler's live sessions.
    ///
    /// A host uses this to force-close a *completed* session it is done with
    /// (or to sweep ahead of teardown) with
    /// [`cleanup_agent`](ExternalSessionRegistry::cleanup_agent) /
    /// [`cleanup`](ExternalSessionRegistry::cleanup). Cancelled or failed
    /// facade drives are already swept automatically through
    /// [`cleanup_agent`](ExternalSessionHandler::cleanup_agent) (M3-2); the
    /// machine never emits a shutdown effect, so completed-session cleanup
    /// stays host-driven.
    #[must_use]
    pub fn registry(&self) -> &Arc<ExternalSessionRegistry> {
        &self.registry
    }

    /// Resolves the live handle and advances it one decision point, folding both
    /// a `get_or_start` failure and an `advance` failure into a family-aligned
    /// [`ExternalSessionResult`].
    async fn advance(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> ExternalSessionResult {
        let handle = match self
            .registry
            .get_or_start(request, ctx, self.sink.clone())
            .await
        {
            Ok(handle) => handle,
            Err(error) => return Err::<RuntimeDecisionPoint, _>(error).into(),
        };
        let point = {
            let mut session = handle.lock().await;
            session.advance(&request.input, ctx).await
        };
        point.into()
    }
}

#[async_trait]
impl ExternalSessionHandler for RegistryExternalSessionHandler {
    async fn fulfill(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
    ) -> RequirementResult {
        RequirementResult::ExternalSession(Box::new(self.advance(request, ctx).await))
    }

    async fn cleanup_agent(&self, agent_id: AgentId) -> Vec<ExternalSessionShutdown> {
        self.registry.cleanup_agent(agent_id).await
    }
}

#[cfg(test)]
mod tests;
