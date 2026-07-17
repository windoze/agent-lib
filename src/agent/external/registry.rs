//! Live-session registry for managed external runtimes.
//!
//! An [`ExternalRuntimeAdapter`](super::ExternalRuntimeAdapter) starts and
//! resumes [`ExternalRuntimeSession`](super::ExternalRuntimeSession)s, but a
//! managed run needs somewhere to *keep* those live handles between decision
//! points, look them up on a follow-up
//! [`ExternalSessionRequest`](super::ExternalSessionRequest), and sweep them on
//! cancel. [`ExternalSessionRegistry`] is that owner (design §11.2).
//!
//! # Why it is not in `ExternalAgentState`
//!
//! Live session handles hold real IO (a CLI child, an SDK client, a reader
//! task). They are deliberately kept out of the serializable
//! [`ExternalAgentState`](super::ExternalAgentState): only *resumable facts*
//! ([`ExternalSessionRef`]) are persisted, and the registry rebuilds live
//! handles beside that state. A restored driver either reattaches to a still-live
//! handle or asks the adapter to [`resume`](super::ExternalRuntimeAdapter::resume)
//! from the persisted [`ExternalSessionRef`]; an unknown, unresumable session
//! fails loudly with [`ExternalAgentError::ResumeUnavailable`] rather than
//! silently starting a fresh one.
//!
//! # Cancellation sweep
//!
//! Cancelling an external agent is never-resume (design §6.4): the driver
//! abandons the continuation, so the machine can never emit a graceful
//! [`Shutdown`](super::ExternalSessionInput::Shutdown). The registry therefore
//! owns the force-close via [`cleanup`](ExternalSessionRegistry::cleanup) and
//! [`cleanup_agent`](ExternalSessionRegistry::cleanup_agent), each returning the
//! [`ExternalSessionShutdown`] disposition a scheduler records to decide whether
//! the worktree is safe to reuse.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

use crate::agent::{AgentId, RunContext};

use super::{
    ExternalAgentError, ExternalEventSink, ExternalRuntimeAdapter, ExternalRuntimeCapabilities,
    ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionRef, ExternalSessionRequest,
    ExternalSessionShutdown,
};

/// A shared handle to one live [`ExternalRuntimeSession`].
///
/// The [`AsyncMutex`](tokio::sync::Mutex) serializes the `&mut self`
/// [`advance`](super::ExternalRuntimeSession::advance) /
/// [`shutdown`](super::ExternalRuntimeSession::shutdown) calls a caller makes
/// across `await` points, and the [`Arc`] lets the registry keep a copy while a
/// handler drives another. Cloning a handle is cheap and shares the same live
/// session.
pub type LiveSessionHandle = Arc<AsyncMutex<Box<dyn ExternalRuntimeSession>>>;

/// Registry key for a live session: an agent plus its runtime-assigned id.
///
/// A session is only findable once the adapter has assigned it a
/// [`session_id`](ExternalSessionRef::session_id); a reference without one cannot
/// name a live handle and is treated as unknown.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LiveSessionKey {
    agent_id: AgentId,
    session_id: String,
}

impl LiveSessionKey {
    /// Builds a key for `agent_id` and `session`, or `None` when the reference
    /// carries no [`session_id`](ExternalSessionRef::session_id).
    fn new(agent_id: AgentId, session: &ExternalSessionRef) -> Option<Self> {
        session.session_id.as_ref().map(|session_id| Self {
            agent_id,
            session_id: session_id.clone(),
        })
    }
}

/// Why registering a freshly created live session failed.
///
/// This is intentionally a small, local error: the registry's synchronous
/// [`register`](ExternalSessionRegistry::register) helper must not carry the
/// large classified [`ExternalAgentError`] across a bare `Result` boundary.
/// [`get_or_start`](ExternalSessionRegistry::get_or_start) lifts it into the
/// public error from inside its async frame via [`into_external`](Self::into_external).
enum RegisterError {
    /// The adapter returned a session with no runtime-assigned session id, so it
    /// could never be found again.
    MissingSessionId,
}

impl RegisterError {
    /// Lifts this registration failure into the public classified error.
    fn into_external(self, kind: ExternalRuntimeKind) -> ExternalAgentError {
        match self {
            RegisterError::MissingSessionId => ExternalAgentError::Protocol {
                detail: format!("{kind:?} adapter returned a session without a session id"),
            },
        }
    }
}

/// Owns the live external-runtime sessions for one adapter.
///
/// The registry maps `(agent_id, session_id)` to a live
/// [`LiveSessionHandle`] and drives the create/reattach/resume decision on each
/// [`ExternalSessionRequest`] through [`get_or_start`](Self::get_or_start). It
/// keeps no serializable state, so it never appears in
/// [`ExternalAgentState`](super::ExternalAgentState).
///
/// The adapter is shared as `Arc<dyn ExternalRuntimeAdapter>`, so a registry is
/// runtime-agnostic and a host can build one per runtime kind.
pub struct ExternalSessionRegistry {
    adapter: Arc<dyn ExternalRuntimeAdapter>,
    live: Mutex<HashMap<LiveSessionKey, LiveSessionHandle>>,
}

impl ExternalSessionRegistry {
    /// Creates a registry backed by `adapter` with no live sessions.
    #[must_use]
    pub fn new(adapter: Arc<dyn ExternalRuntimeAdapter>) -> Self {
        Self {
            adapter,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the runtime kind this registry's adapter drives.
    #[must_use]
    pub fn kind(&self) -> ExternalRuntimeKind {
        self.adapter.kind()
    }

    /// Returns the managed features this registry's adapter reports.
    #[must_use]
    pub fn capabilities(&self) -> ExternalRuntimeCapabilities {
        self.adapter.capabilities()
    }

    /// Returns the number of live sessions currently registered.
    #[must_use]
    pub fn live_len(&self) -> usize {
        self.live.lock().expect("registry mutex poisoned").len()
    }

    /// Returns the live handle for `session` without starting or resuming it.
    ///
    /// This is a pure lookup: it returns `None` when no live handle is
    /// registered for `(agent_id, session_id)` or when `session` carries no
    /// session id.
    #[must_use]
    pub fn get(
        &self,
        agent_id: AgentId,
        session: &ExternalSessionRef,
    ) -> Option<LiveSessionHandle> {
        let key = LiveSessionKey::new(agent_id, session)?;
        self.live
            .lock()
            .expect("registry mutex poisoned")
            .get(&key)
            .cloned()
    }

    /// Resolves the live session for `request`, starting or resuming as needed.
    ///
    /// The decision follows [`request.session`](ExternalSessionRequest::session):
    ///
    /// - **absent** (a first `Start`): the adapter starts a fresh session, which
    ///   is registered under its runtime-assigned id and returned.
    /// - **present with a live handle**: that handle is reattached to and
    ///   returned, so a follow-up input reuses the same live IO.
    /// - **present with no live handle**: the adapter resumes the session when it
    ///   reports [`resume`](super::ExternalRuntimeCapabilities::resume) support;
    ///   otherwise the session is unknown and unresumable.
    ///
    /// `sink` is forwarded to the adapter when a session is started or resumed so
    /// a host can tail live observations; it is ignored when reattaching to an
    /// existing handle whose sink was already wired at creation.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::ResumeUnavailable`] when `request` names a
    /// session that is neither live nor resumable, or any classified adapter
    /// error from starting or resuming.
    pub async fn get_or_start(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<LiveSessionHandle, ExternalAgentError> {
        match &request.session {
            None => {
                let session = self.adapter.start(request, ctx, sink).await?;
                self.register(request.agent_id, session)
                    .map_err(|err| err.into_external(self.adapter.kind()))
            }
            Some(session_ref) => {
                if let Some(existing) = self.get(request.agent_id, session_ref) {
                    return Ok(existing);
                }
                if self.adapter.capabilities().resume {
                    let session = self.adapter.resume(session_ref, request, ctx, sink).await?;
                    self.register(request.agent_id, session)
                        .map_err(|err| err.into_external(self.adapter.kind()))
                } else {
                    Err(ExternalAgentError::ResumeUnavailable {
                        session: session_ref.clone(),
                        detail: format!(
                            "no live {:?} session registered and adapter does not support resume",
                            self.adapter.kind()
                        ),
                    })
                }
            }
        }
    }

    /// Registers a freshly started or resumed session under its assigned id.
    ///
    /// Returns [`ExternalAgentError::Protocol`] when the session exposes no
    /// [`session_id`](ExternalSessionRef::session_id), because such a handle
    /// could never be found again; the unkeyed session is dropped, releasing its
    /// live IO through the handle's own teardown. When another task registered
    /// the same key concurrently, the already-stored handle is returned so no
    /// live session is leaked.
    ///
    /// This borrows the caller's async frame (it is only reached from
    /// [`get_or_start`](Self::get_or_start)) so the large classified error stays
    /// off a bare synchronous `Result` boundary.
    fn register(
        &self,
        agent_id: AgentId,
        session: Box<dyn ExternalRuntimeSession>,
    ) -> Result<LiveSessionHandle, RegisterError> {
        let session_ref = session.session_ref();
        let Some(key) = LiveSessionKey::new(agent_id, &session_ref) else {
            return Err(RegisterError::MissingSessionId);
        };
        let handle: LiveSessionHandle = Arc::new(AsyncMutex::new(session));
        let mut live = self.live.lock().expect("registry mutex poisoned");
        // Re-check under the lock so a concurrent start does not leak a handle.
        if let Some(existing) = live.get(&key) {
            return Ok(existing.clone());
        }
        live.insert(key, handle.clone());
        Ok(handle)
    }

    /// Closes and deregisters the live session named by `session`.
    ///
    /// The handle is removed from the registry first, then the session is closed
    /// and its [`ExternalSessionShutdown`] disposition returned. A session that
    /// is not registered (already swept, never started, or missing a session id)
    /// yields [`ExternalSessionShutdown::Graceful`] because there is nothing left
    /// to close.
    pub async fn cleanup(
        &self,
        agent_id: AgentId,
        session: &ExternalSessionRef,
    ) -> ExternalSessionShutdown {
        let Some(key) = LiveSessionKey::new(agent_id, session) else {
            return ExternalSessionShutdown::Graceful;
        };
        let handle = self
            .live
            .lock()
            .expect("registry mutex poisoned")
            .remove(&key);
        match handle {
            Some(handle) => handle.lock().await.shutdown().await,
            None => ExternalSessionShutdown::Graceful,
        }
    }

    /// Cancel-sweeps every live session belonging to `agent_id`.
    ///
    /// Each matching handle is removed and closed; the returned dispositions are
    /// ordered by session id for determinism. An agent with no live sessions
    /// yields an empty vector.
    pub async fn cleanup_agent(&self, agent_id: AgentId) -> Vec<ExternalSessionShutdown> {
        let mut handles: Vec<(String, LiveSessionHandle)> = {
            let mut live = self.live.lock().expect("registry mutex poisoned");
            let keys: Vec<LiveSessionKey> = live
                .keys()
                .filter(|key| key.agent_id == agent_id)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| {
                    live.remove(&key)
                        .map(|handle| (key.session_id.clone(), handle))
                })
                .collect()
        };
        handles.sort_by(|(left, _), (right, _)| left.cmp(right));

        let mut dispositions = Vec::with_capacity(handles.len());
        for (_, handle) in handles {
            dispositions.push(handle.lock().await.shutdown().await);
        }
        dispositions
    }
}

impl std::fmt::Debug for ExternalSessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalSessionRegistry")
            .field("kind", &self.adapter.kind())
            .field("live_len", &self.live_len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use crate::agent::external::{
        ExternalAgentError, ExternalAgentOutput, ExternalEventSink, ExternalPermissionMode,
        ExternalRuntimeAdapter, ExternalRuntimeCapabilities, ExternalRuntimeKind,
        ExternalRuntimeSession, ExternalSessionInput, ExternalSessionPolicy, ExternalSessionRef,
        ExternalSessionRequest, ExternalSessionShutdown, ExternalStreamPolicy,
        RuntimeDecisionPoint, WorktreeIsolation,
    };
    use crate::agent::spec::WorktreeRef;
    use crate::agent::{AgentId, BudgetLimits, RunContext, RunId, TraceNodeId};

    use super::ExternalSessionRegistry;

    fn agent_id() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f0"
            .parse()
            .expect("agent id")
    }

    fn other_agent_id() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890f5"
            .parse()
            .expect("agent id")
    }

    fn run_context() -> RunContext {
        let run_id: RunId = "018f0d9c-7b6a-7c12-8f31-1234567890e0"
            .parse()
            .expect("run id");
        let trace_root = TraceNodeId::new("external-runtime-registry-root");
        RunContext::new_root(run_id, BudgetLimits::unbounded(), trace_root)
    }

    fn policy() -> ExternalSessionPolicy {
        ExternalSessionPolicy {
            permission_mode: ExternalPermissionMode::AcceptEdits,
            isolation: WorktreeIsolation::EphemeralGitWorktree,
            max_turns: Some(8),
            stream_events: ExternalStreamPolicy::Buffered,
        }
    }

    fn start_request(agent: AgentId) -> ExternalSessionRequest {
        ExternalSessionRequest {
            agent_id: agent,
            runtime: ExternalRuntimeKind::ClaudeCode,
            worktree: WorktreeRef::new("/repo/agent-lib"),
            session: None,
            input: ExternalSessionInput::Start {
                prompt: "do the thing".to_owned(),
            },
            tools: Vec::new(),
            policy: policy(),
        }
    }

    fn continue_request(agent: AgentId, session_id: &str) -> ExternalSessionRequest {
        ExternalSessionRequest {
            agent_id: agent,
            runtime: ExternalRuntimeKind::ClaudeCode,
            worktree: WorktreeRef::new("/repo/agent-lib"),
            session: Some(session_ref(session_id)),
            input: ExternalSessionInput::Continue {
                message: "keep going".to_owned(),
            },
            tools: Vec::new(),
            policy: policy(),
        }
    }

    fn session_ref(session_id: &str) -> ExternalSessionRef {
        ExternalSessionRef {
            runtime: ExternalRuntimeKind::ClaudeCode,
            session_id: Some(session_id.to_owned()),
            transcript_ref: None,
            resume_token: None,
            last_event_seq: None,
        }
    }

    /// Records whether it was force-closed and reports it on `shutdown`.
    struct MockSession {
        session_id: String,
        shutdown_disposition: ExternalSessionShutdown,
        shutdowns: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ExternalRuntimeSession for MockSession {
        fn session_ref(&self) -> ExternalSessionRef {
            session_ref(&self.session_id)
        }

        async fn advance(
            &mut self,
            _input: &ExternalSessionInput,
            _ctx: &RunContext,
        ) -> Result<RuntimeDecisionPoint, ExternalAgentError> {
            Ok(RuntimeDecisionPoint::Completed {
                session: session_ref(&self.session_id),
                output: ExternalAgentOutput {
                    summary: "done".to_owned(),
                    artifacts: Vec::new(),
                    usage: None,
                    cost_micros: None,
                },
                observations: Vec::new(),
            })
        }

        async fn shutdown(&mut self) -> ExternalSessionShutdown {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            self.shutdown_disposition
        }
    }

    /// Adapter that assigns sequential session ids and can be configured to
    /// support or refuse resume.
    struct MockAdapter {
        resume_supported: bool,
        next_session: AtomicUsize,
        starts: Arc<AtomicUsize>,
        resumes: Arc<AtomicUsize>,
        shutdowns: Arc<AtomicUsize>,
        shutdown_disposition: ExternalSessionShutdown,
    }

    impl MockAdapter {
        fn new(resume_supported: bool) -> Self {
            Self {
                resume_supported,
                next_session: AtomicUsize::new(0),
                starts: Arc::new(AtomicUsize::new(0)),
                resumes: Arc::new(AtomicUsize::new(0)),
                shutdowns: Arc::new(AtomicUsize::new(0)),
                shutdown_disposition: ExternalSessionShutdown::Graceful,
            }
        }

        fn make_session(&self, session_id: String) -> Box<dyn ExternalRuntimeSession> {
            Box::new(MockSession {
                session_id,
                shutdown_disposition: self.shutdown_disposition,
                shutdowns: Arc::clone(&self.shutdowns),
            })
        }
    }

    #[async_trait]
    impl ExternalRuntimeAdapter for MockAdapter {
        fn kind(&self) -> ExternalRuntimeKind {
            ExternalRuntimeKind::ClaudeCode
        }

        fn capabilities(&self) -> ExternalRuntimeCapabilities {
            let mut caps = ExternalRuntimeCapabilities::none(ExternalRuntimeKind::ClaudeCode);
            caps.resume = self.resume_supported;
            caps
        }

        async fn start(
            &self,
            _request: &ExternalSessionRequest,
            _ctx: &RunContext,
            _sink: Option<Arc<dyn ExternalEventSink>>,
        ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let n = self.next_session.fetch_add(1, Ordering::SeqCst);
            Ok(self.make_session(format!("session-{n}")))
        }

        async fn resume(
            &self,
            session: &ExternalSessionRef,
            _request: &ExternalSessionRequest,
            _ctx: &RunContext,
            _sink: Option<Arc<dyn ExternalEventSink>>,
        ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
            self.resumes.fetch_add(1, Ordering::SeqCst);
            let session_id = session
                .session_id
                .clone()
                .expect("resume called with a session id");
            Ok(self.make_session(session_id))
        }
    }

    #[tokio::test]
    async fn external_runtime_registry_start_registers_and_get_finds_live_handle() {
        // A fresh Start goes through the adapter, is registered under its
        // runtime-assigned id, and is then findable by a pure `get`.
        let adapter = Arc::new(MockAdapter::new(false));
        let starts = Arc::clone(&adapter.starts);
        let registry = ExternalSessionRegistry::new(adapter);
        let ctx = run_context();

        assert_eq!(registry.live_len(), 0);
        let started = registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("start succeeds");
        assert_eq!(registry.live_len(), 1);
        assert_eq!(starts.load(Ordering::SeqCst), 1);

        let session_id = started.lock().await.session_ref().session_id.expect("id");
        let found = registry
            .get(agent_id(), &session_ref(&session_id))
            .expect("live handle is findable");
        assert!(
            Arc::ptr_eq(&started, &found),
            "get returns the same live handle"
        );
    }

    #[tokio::test]
    async fn external_runtime_registry_reattaches_to_live_handle_on_continue() {
        // A Continue that names a live session reattaches to the same handle
        // instead of starting or resuming another.
        let adapter = Arc::new(MockAdapter::new(false));
        let starts = Arc::clone(&adapter.starts);
        let resumes = Arc::clone(&adapter.resumes);
        let registry = ExternalSessionRegistry::new(adapter);
        let ctx = run_context();

        let started = registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("start succeeds");
        let session_id = started.lock().await.session_ref().session_id.expect("id");

        let reattached = registry
            .get_or_start(&continue_request(agent_id(), &session_id), &ctx, None)
            .await
            .expect("reattach succeeds");

        assert!(
            Arc::ptr_eq(&started, &reattached),
            "continue reuses the live handle"
        );
        assert_eq!(registry.live_len(), 1);
        assert_eq!(starts.load(Ordering::SeqCst), 1, "no second start");
        assert_eq!(
            resumes.load(Ordering::SeqCst),
            0,
            "no resume for live handle"
        );
    }

    #[tokio::test]
    async fn external_runtime_registry_resumes_when_adapter_supports_it() {
        // With no live handle but a resume-capable adapter, a Continue resumes
        // the session from its persisted reference and registers the handle.
        let adapter = Arc::new(MockAdapter::new(true));
        let resumes = Arc::clone(&adapter.resumes);
        let registry = ExternalSessionRegistry::new(adapter);
        let ctx = run_context();

        let resumed = registry
            .get_or_start(&continue_request(agent_id(), "persisted-1"), &ctx, None)
            .await
            .expect("resume succeeds");

        assert_eq!(resumes.load(Ordering::SeqCst), 1);
        assert_eq!(registry.live_len(), 1);
        assert_eq!(
            resumed.lock().await.session_ref().session_id.as_deref(),
            Some("persisted-1"),
        );
        // The resumed handle is now live and findable.
        assert!(
            registry
                .get(agent_id(), &session_ref("persisted-1"))
                .is_some()
        );
    }

    #[tokio::test]
    async fn external_runtime_registry_unknown_session_maps_resume_unavailable() {
        // No live handle and an adapter that cannot resume: the session is
        // unknown and unresumable, surfaced as a classified ResumeUnavailable.
        let adapter = Arc::new(MockAdapter::new(false));
        let registry = ExternalSessionRegistry::new(adapter);
        let ctx = run_context();

        let error = match registry
            .get_or_start(&continue_request(agent_id(), "ghost-1"), &ctx, None)
            .await
        {
            Ok(_) => panic!("unknown session should not resume"),
            Err(error) => error,
        };

        match error {
            ExternalAgentError::ResumeUnavailable { session, detail } => {
                assert_eq!(session.session_id.as_deref(), Some("ghost-1"));
                assert!(!detail.is_empty());
            }
            other => panic!("expected ResumeUnavailable, got {other:?}"),
        }
        assert_eq!(registry.live_len(), 0, "no handle registered on failure");
    }

    #[tokio::test]
    async fn external_runtime_registry_cleanup_removes_and_closes_handle() {
        // Cleanup closes the live session, reports its disposition, and removes
        // the handle so a later lookup misses.
        let adapter = Arc::new(MockAdapter::new(false));
        let shutdowns = Arc::clone(&adapter.shutdowns);
        let registry = ExternalSessionRegistry::new(adapter);
        let ctx = run_context();

        let started = registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("start succeeds");
        let session_id = started.lock().await.session_ref().session_id.expect("id");
        assert_eq!(registry.live_len(), 1);

        let disposition = registry
            .cleanup(agent_id(), &session_ref(&session_id))
            .await;
        assert_eq!(disposition, ExternalSessionShutdown::Graceful);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
        assert_eq!(registry.live_len(), 0);
        assert!(
            registry
                .get(agent_id(), &session_ref(&session_id))
                .is_none(),
            "cleanup removed the live handle"
        );
    }

    #[tokio::test]
    async fn external_runtime_registry_cleanup_missing_session_is_graceful() {
        // Cleaning a session that was never registered is a graceful no-op.
        let adapter = Arc::new(MockAdapter::new(false));
        let registry = ExternalSessionRegistry::new(adapter);

        let disposition = registry.cleanup(agent_id(), &session_ref("absent")).await;
        assert_eq!(disposition, ExternalSessionShutdown::Graceful);
        assert_eq!(registry.live_len(), 0);
    }

    #[tokio::test]
    async fn external_runtime_registry_cleanup_agent_sweeps_only_that_agent() {
        // cleanup_agent force-closes every session for one agent and leaves other
        // agents' sessions live (the never-resume cancel sweep).
        let adapter = Arc::new(MockAdapter::new(false));
        let shutdowns = Arc::clone(&adapter.shutdowns);
        let registry = ExternalSessionRegistry::new(adapter);
        let ctx = run_context();

        registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("first start");
        registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("second start");
        registry
            .get_or_start(&start_request(other_agent_id()), &ctx, None)
            .await
            .expect("other-agent start");
        assert_eq!(registry.live_len(), 3);

        let dispositions = registry.cleanup_agent(agent_id()).await;
        assert_eq!(dispositions.len(), 2);
        assert!(
            dispositions
                .iter()
                .all(|d| *d == ExternalSessionShutdown::Graceful)
        );
        assert_eq!(shutdowns.load(Ordering::SeqCst), 2);
        assert_eq!(registry.live_len(), 1, "other agent's session survives");
    }

    #[tokio::test]
    async fn external_runtime_registry_cleanup_agent_without_sessions_is_empty() {
        // Sweeping an agent with no live sessions yields no dispositions.
        let adapter = Arc::new(MockAdapter::new(false));
        let registry = ExternalSessionRegistry::new(adapter);

        let dispositions = registry.cleanup_agent(agent_id()).await;
        assert!(dispositions.is_empty());
    }
}
