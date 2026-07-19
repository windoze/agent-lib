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
//!
//! # Worktree isolation (M2-7 / M-PROM-5)
//!
//! The registry is the single choke point every managed session start/resume
//! flows through, so it is also where
//! [`ExternalSessionPolicy::isolation`](super::ExternalSessionPolicy) becomes
//! real: before the adapter starts or resumes a session, the registry applies
//! the request's isolation level through its
//! [`WorktreeManager`](super::WorktreeManager) (a
//! [`GitWorktreeManager`](super::GitWorktreeManager) by default) and hands the
//! prepared path to the adapter as the session's working directory
//! ([`ExternalSessionRequest::session_dir`]). Each live session's
//! [`PreparedWorktree`] is remembered, and
//! [`cleanup`](ExternalSessionRegistry::cleanup) /
//! [`cleanup_agent`](ExternalSessionRegistry::cleanup_agent) feed the session's
//! shutdown disposition into
//! [`WorktreeManager::cleanup`](super::WorktreeManager::cleanup) so an
//! ephemeral worktree is removed only when the session closed without residual
//! side effects.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

use crate::agent::{AgentId, RunContext};

use super::{
    ExternalAgentError, ExternalEventSink, ExternalRuntimeAdapter, ExternalRuntimeCapabilities,
    ExternalRuntimeKind, ExternalRuntimeSession, ExternalSessionRef, ExternalSessionRequest,
    ExternalSessionShutdown, GitWorktreeManager, PreparedWorktree, WorktreeManager,
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

/// A live session plus the worktree prepared for it.
///
/// The `prepared` half is remembered so the registry can hand it back to the
/// [`WorktreeManager`] with the session's shutdown disposition when the session
/// is swept (M2-7); it plays no part in lookups.
struct LiveEntry {
    handle: LiveSessionHandle,
    prepared: PreparedWorktree,
}

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
/// runtime-agnostic and a host can build one per runtime kind. The registry
/// also owns the [`WorktreeManager`] that turns
/// [`ExternalSessionPolicy::isolation`](super::ExternalSessionPolicy) into a
/// concrete session working directory: every start/resume first
/// [`prepare`](WorktreeManager::prepare)s the request's worktree and passes the
/// prepared path to the adapter as
/// [`ExternalSessionRequest::session_dir`], and every sweep hands the recorded
/// [`PreparedWorktree`] back to [`cleanup`](WorktreeManager::cleanup) with the
/// session's shutdown disposition (M2-7 / M-PROM-5).
pub struct ExternalSessionRegistry {
    adapter: Arc<dyn ExternalRuntimeAdapter>,
    worktrees: Arc<dyn WorktreeManager>,
    live: Mutex<HashMap<LiveSessionKey, LiveEntry>>,
}

impl ExternalSessionRegistry {
    /// Creates a registry backed by `adapter` with no live sessions.
    ///
    /// Worktree preparation uses a default
    /// [`GitWorktreeManager`](super::GitWorktreeManager) rooted under the OS
    /// temp dir; inject a different manager with
    /// [`with_worktree_manager`](Self::with_worktree_manager).
    #[must_use]
    pub fn new(adapter: Arc<dyn ExternalRuntimeAdapter>) -> Self {
        Self::with_worktree_manager(adapter, Arc::new(GitWorktreeManager::new()))
    }

    /// Creates a registry backed by `adapter` that prepares and cleans up
    /// session worktrees through `worktrees`.
    #[must_use]
    pub fn with_worktree_manager(
        adapter: Arc<dyn ExternalRuntimeAdapter>,
        worktrees: Arc<dyn WorktreeManager>,
    ) -> Self {
        Self {
            adapter,
            worktrees,
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
        self.live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .len()
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
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&key)
            .map(|entry| entry.handle.clone())
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
    /// Before the adapter starts or resumes, the request's
    /// [`isolation`](super::ExternalSessionPolicy) level is applied through the
    /// registry's [`WorktreeManager`] and the prepared path is handed to the
    /// adapter as the session's working directory
    /// ([`ExternalSessionRequest::session_dir`]). Reattaching to a live handle
    /// reuses the worktree prepared when that handle was created.
    ///
    /// # Errors
    ///
    /// Returns [`ExternalAgentError::ResumeUnavailable`] when `request` names a
    /// session that is neither live nor resumable, [`ExternalAgentError::Launch`]
    /// (start) / [`ResumeUnavailable`](ExternalAgentError::ResumeUnavailable)
    /// (resume) when worktree preparation fails, or any classified adapter
    /// error from starting or resuming.
    pub async fn get_or_start(
        &self,
        request: &ExternalSessionRequest,
        ctx: &RunContext,
        sink: Option<Arc<dyn ExternalEventSink>>,
    ) -> Result<LiveSessionHandle, ExternalAgentError> {
        match &request.session {
            None => {
                let (effective, prepared) = self.prepare(request).await?;
                match self.adapter.start(&effective, ctx, sink).await {
                    Ok(session) => self
                        .register(request.agent_id, prepared, session)
                        .map_err(|err| err.into_external(self.adapter.kind())),
                    Err(error) => {
                        self.discard_prepared(prepared).await;
                        Err(error)
                    }
                }
            }
            Some(session_ref) => {
                if let Some(existing) = self.get(request.agent_id, session_ref) {
                    return Ok(existing);
                }
                if self.adapter.capabilities().resume {
                    let (effective, prepared) = self.prepare(request).await?;
                    match self
                        .adapter
                        .resume(session_ref, &effective, ctx, sink)
                        .await
                    {
                        Ok(session) => self
                            .register(request.agent_id, prepared, session)
                            .map_err(|err| err.into_external(self.adapter.kind())),
                        Err(error) => {
                            self.discard_prepared(prepared).await;
                            Err(error)
                        }
                    }
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

    /// Applies the request's isolation level, returning the effective request
    /// (with [`ExternalSessionRequest::session_dir`] set to the prepared path)
    /// and the [`PreparedWorktree`] to remember for cleanup.
    ///
    /// A preparation failure is classified along the start/resume axis: a fresh
    /// start reports [`ExternalAgentError::Launch`], a resume reports
    /// [`ExternalAgentError::ResumeUnavailable`] against the session it tried to
    /// revive.
    async fn prepare(
        &self,
        request: &ExternalSessionRequest,
    ) -> Result<(ExternalSessionRequest, PreparedWorktree), ExternalAgentError> {
        let prepared = self
            .worktrees
            .prepare(
                request.agent_id,
                &request.worktree,
                request.policy.isolation,
            )
            .await
            .map_err(|error| match &request.session {
                None => ExternalAgentError::Launch {
                    runtime: self.adapter.kind(),
                    detail: format!("failed preparing the session worktree: {error}"),
                },
                Some(session_ref) => ExternalAgentError::ResumeUnavailable {
                    session: session_ref.clone(),
                    detail: format!("failed preparing the session worktree: {error}"),
                },
            })?;
        let mut effective = request.clone();
        effective.session_dir = Some(prepared.worktree().clone());
        Ok((effective, prepared))
    }

    /// Best-effort discard of a worktree whose session never started.
    ///
    /// The adapter refused the start/resume after preparation succeeded, so no
    /// runtime ever ran in the prepared tree: it is cleaned up with a
    /// [`Graceful`](ExternalSessionShutdown::Graceful) disposition, which removes
    /// an ephemeral tree instead of leaking it. A cleanup failure here is
    /// deliberately swallowed — the original adapter error is the signal the
    /// caller needs.
    async fn discard_prepared(&self, prepared: PreparedWorktree) {
        let _ = self
            .worktrees
            .cleanup(prepared, ExternalSessionShutdown::Graceful)
            .await;
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
        prepared: PreparedWorktree,
        session: Box<dyn ExternalRuntimeSession>,
    ) -> Result<LiveSessionHandle, RegisterError> {
        let session_ref = session.session_ref();
        let Some(key) = LiveSessionKey::new(agent_id, &session_ref) else {
            return Err(RegisterError::MissingSessionId);
        };
        let handle: LiveSessionHandle = Arc::new(AsyncMutex::new(session));
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // Re-check under the lock so a concurrent start does not leak a handle.
        if let Some(existing) = live.get(&key) {
            return Ok(existing.handle.clone());
        }
        live.insert(
            key,
            LiveEntry {
                handle: handle.clone(),
                prepared,
            },
        );
        Ok(handle)
    }

    /// Closes and deregisters the live session named by `session`.
    ///
    /// The handle is removed from the registry first, then the session is closed
    /// and its [`ExternalSessionShutdown`] disposition returned. A session that
    /// is not registered (already swept, never started, or missing a session id)
    /// yields [`ExternalSessionShutdown::Graceful`] because there is nothing left
    /// to close.
    ///
    /// After the session closes, its recorded [`PreparedWorktree`] is handed to
    /// the registry's [`WorktreeManager`] with the session's disposition, so an
    /// ephemeral worktree is removed only when the session closed cleanly
    /// (M2-7). A worktree-cleanup failure escalates the returned disposition to
    /// [`Failed`](ExternalSessionShutdown::Failed): the tree may have been left
    /// behind, which is a residual side effect.
    pub async fn cleanup(
        &self,
        agent_id: AgentId,
        session: &ExternalSessionRef,
    ) -> ExternalSessionShutdown {
        let Some(key) = LiveSessionKey::new(agent_id, session) else {
            return ExternalSessionShutdown::Graceful;
        };
        let entry = self
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&key);
        match entry {
            Some(entry) => {
                let disposition = entry.handle.lock().await.shutdown().await;
                self.sweep_worktree(entry.prepared, disposition).await
            }
            None => ExternalSessionShutdown::Graceful,
        }
    }

    /// Cleans up a session's prepared worktree, escalating the disposition to
    /// [`Failed`](ExternalSessionShutdown::Failed) when the cleanup itself fails.
    async fn sweep_worktree(
        &self,
        prepared: PreparedWorktree,
        disposition: ExternalSessionShutdown,
    ) -> ExternalSessionShutdown {
        match self.worktrees.cleanup(prepared, disposition).await {
            Ok(_outcome) => disposition,
            Err(_error) => ExternalSessionShutdown::Failed,
        }
    }

    /// Cancel-sweeps every live session belonging to `agent_id`.
    ///
    /// Each matching handle is removed and closed; the returned dispositions are
    /// ordered by session id for determinism. An agent with no live sessions
    /// yields an empty vector. Each session's prepared worktree is cleaned up
    /// with that session's disposition, exactly as
    /// [`cleanup`](Self::cleanup) does for a single session.
    pub async fn cleanup_agent(&self, agent_id: AgentId) -> Vec<ExternalSessionShutdown> {
        let mut entries: Vec<(String, LiveEntry)> = {
            let mut live = self
                .live
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let keys: Vec<LiveSessionKey> = live
                .keys()
                .filter(|key| key.agent_id == agent_id)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| {
                    live.remove(&key)
                        .map(|entry| (key.session_id.clone(), entry))
                })
                .collect()
        };
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));

        let mut dispositions = Vec::with_capacity(entries.len());
        for (_, entry) in entries {
            let disposition = entry.handle.lock().await.shutdown().await;
            dispositions.push(self.sweep_worktree(entry.prepared, disposition).await);
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use crate::agent::external::{
        ExternalAgentError, ExternalAgentOutput, ExternalEventSink, ExternalPermissionMode,
        ExternalRuntimeAdapter, ExternalRuntimeCapabilities, ExternalRuntimeKind,
        ExternalRuntimeSession, ExternalSessionInput, ExternalSessionPolicy, ExternalSessionRef,
        ExternalSessionRequest, ExternalSessionShutdown, ExternalStreamPolicy, PreparedWorktree,
        RuntimeDecisionPoint, WorktreeCleanupOutcome, WorktreeError, WorktreeIsolation,
        WorktreeManager,
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
            session_dir: None,
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
            session_dir: None,
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
        fail_start: bool,
        /// `session_dir` the adapter observed on each start/resume request, in
        /// call order — proves the registry's prepared path reaches the adapter.
        observed_session_dirs: Arc<Mutex<Vec<Option<WorktreeRef>>>>,
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
                fail_start: false,
                observed_session_dirs: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn make_session(&self, session_id: String) -> Box<dyn ExternalRuntimeSession> {
            Box::new(MockSession {
                session_id,
                shutdown_disposition: self.shutdown_disposition,
                shutdowns: Arc::clone(&self.shutdowns),
            })
        }

        fn observe(&self, request: &ExternalSessionRequest) {
            self.observed_session_dirs
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(request.session_dir.clone());
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
            request: &ExternalSessionRequest,
            _ctx: &RunContext,
            _sink: Option<Arc<dyn ExternalEventSink>>,
        ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.observe(request);
            if self.fail_start {
                return Err(ExternalAgentError::Launch {
                    runtime: ExternalRuntimeKind::ClaudeCode,
                    detail: "stub launch failure".to_owned(),
                });
            }
            let n = self.next_session.fetch_add(1, Ordering::SeqCst);
            Ok(self.make_session(format!("session-{n}")))
        }

        async fn resume(
            &self,
            session: &ExternalSessionRef,
            request: &ExternalSessionRequest,
            _ctx: &RunContext,
            _sink: Option<Arc<dyn ExternalEventSink>>,
        ) -> Result<Box<dyn ExternalRuntimeSession>, ExternalAgentError> {
            self.resumes.fetch_add(1, Ordering::SeqCst);
            self.observe(request);
            let session_id = session
                .session_id
                .clone()
                .expect("resume called with a session id");
            Ok(self.make_session(session_id))
        }
    }

    /// Records every prepare/cleanup call and hands out synthetic prepared paths
    /// under `/prepared/` so tests can watch the registry's worktree wiring
    /// without touching a real filesystem.
    struct StubWorktreeManager {
        prepares: Mutex<Vec<(AgentId, WorktreeRef, WorktreeIsolation)>>,
        cleanups: Mutex<Vec<(WorktreeRef, ExternalSessionShutdown)>>,
        fail_prepare: AtomicBool,
        fail_cleanup: AtomicBool,
    }

    impl StubWorktreeManager {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                prepares: Mutex::new(Vec::new()),
                cleanups: Mutex::new(Vec::new()),
                fail_prepare: AtomicBool::new(false),
                fail_cleanup: AtomicBool::new(false),
            })
        }

        fn prepares(&self) -> Vec<(AgentId, WorktreeRef, WorktreeIsolation)> {
            self.prepares
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }

        fn cleanups(&self) -> Vec<(WorktreeRef, ExternalSessionShutdown)> {
            self.cleanups
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }

        /// The prepared path the `n`-th prepare call handed out.
        fn prepared_path(n: usize) -> WorktreeRef {
            WorktreeRef::new(format!("/prepared/session-{n}"))
        }
    }

    #[async_trait]
    impl WorktreeManager for StubWorktreeManager {
        async fn prepare(
            &self,
            agent_id: AgentId,
            base: &WorktreeRef,
            isolation: WorktreeIsolation,
        ) -> Result<PreparedWorktree, WorktreeError> {
            if self.fail_prepare.load(Ordering::SeqCst) {
                return Err(WorktreeError::Prepare {
                    isolation,
                    path: base.path().to_string_lossy().into_owned(),
                    detail: "stub prepare failure".to_owned(),
                });
            }
            let mut prepares = self
                .prepares
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let path = Self::prepared_path(prepares.len());
            prepares.push((agent_id, base.clone(), isolation));
            Ok(PreparedWorktree::new(agent_id, isolation, path, true).with_base_repo(base.clone()))
        }

        async fn cleanup(
            &self,
            prepared: PreparedWorktree,
            disposition: ExternalSessionShutdown,
        ) -> Result<WorktreeCleanupOutcome, WorktreeError> {
            if self.fail_cleanup.load(Ordering::SeqCst) {
                return Err(WorktreeError::Cleanup {
                    isolation: prepared.isolation(),
                    path: prepared.worktree().path().to_string_lossy().into_owned(),
                    detail: "stub cleanup failure".to_owned(),
                });
            }
            self.cleanups
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push((prepared.worktree().clone(), disposition));
            Ok(WorktreeCleanupOutcome::new(
                prepared.isolation(),
                prepared.worktree().clone(),
                true,
                disposition.leaves_residual_side_effects(),
            ))
        }
    }

    /// Builds a registry over `adapter` whose worktree preparation is watched by
    /// the returned stub.
    fn registry_with_stub(
        adapter: Arc<MockAdapter>,
        stub: Arc<StubWorktreeManager>,
    ) -> ExternalSessionRegistry {
        ExternalSessionRegistry::with_worktree_manager(adapter, stub)
    }

    #[tokio::test]
    async fn external_runtime_registry_start_registers_and_get_finds_live_handle() {
        // A fresh Start goes through the adapter, is registered under its
        // runtime-assigned id, and is then findable by a pure `get`.
        let adapter = Arc::new(MockAdapter::new(false));
        let starts = Arc::clone(&adapter.starts);
        let registry = registry_with_stub(adapter, StubWorktreeManager::new());
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
        let registry = registry_with_stub(adapter, StubWorktreeManager::new());
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
        let registry = registry_with_stub(adapter, StubWorktreeManager::new());
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
        let registry = registry_with_stub(adapter, StubWorktreeManager::new());
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
        let registry = registry_with_stub(adapter, StubWorktreeManager::new());
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
        let registry = registry_with_stub(adapter, StubWorktreeManager::new());

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
        let registry = registry_with_stub(adapter, StubWorktreeManager::new());
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
        let registry = registry_with_stub(adapter, StubWorktreeManager::new());

        let dispositions = registry.cleanup_agent(agent_id()).await;
        assert!(dispositions.is_empty());
    }

    #[tokio::test]
    async fn external_runtime_registry_start_prepares_worktree_and_hands_session_dir_to_adapter() {
        // M2-7: a fresh start applies the request's isolation through the
        // registry's WorktreeManager *before* the adapter runs, and the adapter
        // sees the prepared path as the request's session_dir.
        let adapter = Arc::new(MockAdapter::new(false));
        let observed = Arc::clone(&adapter.observed_session_dirs);
        let stub = StubWorktreeManager::new();
        let registry = registry_with_stub(adapter, Arc::clone(&stub));
        let ctx = run_context();

        registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("start succeeds");

        assert_eq!(
            stub.prepares(),
            vec![(
                agent_id(),
                WorktreeRef::new("/repo/agent-lib"),
                WorktreeIsolation::EphemeralGitWorktree,
            ),],
            "prepare ran with the request's agent, base worktree, and isolation"
        );
        assert_eq!(
            observed
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .as_slice(),
            &[Some(StubWorktreeManager::prepared_path(0))],
            "the adapter received the prepared path as session_dir"
        );
    }

    #[tokio::test]
    async fn external_runtime_registry_reattach_reuses_prepared_worktree() {
        // Reattaching to a live handle must not prepare a second worktree: the
        // session keeps running in the one prepared when it started.
        let adapter = Arc::new(MockAdapter::new(false));
        let stub = StubWorktreeManager::new();
        let registry = registry_with_stub(adapter, Arc::clone(&stub));
        let ctx = run_context();

        let started = registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("start succeeds");
        let session_id = started.lock().await.session_ref().session_id.expect("id");
        registry
            .get_or_start(&continue_request(agent_id(), &session_id), &ctx, None)
            .await
            .expect("reattach succeeds");

        assert_eq!(
            stub.prepares().len(),
            1,
            "reattaching to a live handle reuses the prepared worktree"
        );
    }

    #[tokio::test]
    async fn external_runtime_registry_resume_prepares_worktree_and_hands_session_dir_to_adapter() {
        // A resume (no live handle) also runs through preparation: the revived
        // session gets a fresh prepared worktree handed to the adapter.
        let adapter = Arc::new(MockAdapter::new(true));
        let observed = Arc::clone(&adapter.observed_session_dirs);
        let stub = StubWorktreeManager::new();
        let registry = registry_with_stub(adapter, Arc::clone(&stub));
        let ctx = run_context();

        registry
            .get_or_start(&continue_request(agent_id(), "persisted-1"), &ctx, None)
            .await
            .expect("resume succeeds");

        assert_eq!(stub.prepares().len(), 1, "resume prepares a worktree");
        assert_eq!(
            observed
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .as_slice(),
            &[Some(StubWorktreeManager::prepared_path(0))],
            "the adapter received the prepared path as session_dir"
        );
    }

    #[tokio::test]
    async fn external_runtime_registry_prepare_failure_on_start_maps_to_launch() {
        // A worktree that cannot be prepared fails the start loudly with a
        // classified Launch error, before the adapter ever runs.
        let adapter = Arc::new(MockAdapter::new(false));
        let starts = Arc::clone(&adapter.starts);
        let stub = StubWorktreeManager::new();
        stub.fail_prepare.store(true, Ordering::SeqCst);
        let registry = registry_with_stub(adapter, stub);
        let ctx = run_context();

        let error = match registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
        {
            Ok(_) => panic!("start should fail when worktree preparation fails"),
            Err(error) => error,
        };

        match error {
            ExternalAgentError::Launch { runtime, detail } => {
                assert_eq!(runtime, ExternalRuntimeKind::ClaudeCode);
                assert!(detail.contains("worktree"), "detail names the cause");
            }
            other => panic!("expected Launch, got {other:?}"),
        }
        assert_eq!(starts.load(Ordering::SeqCst), 0, "adapter never ran");
        assert_eq!(registry.live_len(), 0);
    }

    #[tokio::test]
    async fn external_runtime_registry_prepare_failure_on_resume_maps_to_resume_unavailable() {
        // The same preparation failure on a resume classifies along the resume
        // axis, against the session that was being revived.
        let adapter = Arc::new(MockAdapter::new(true));
        let resumes = Arc::clone(&adapter.resumes);
        let stub = StubWorktreeManager::new();
        stub.fail_prepare.store(true, Ordering::SeqCst);
        let registry = registry_with_stub(adapter, stub);
        let ctx = run_context();

        let error = match registry
            .get_or_start(&continue_request(agent_id(), "persisted-1"), &ctx, None)
            .await
        {
            Ok(_) => panic!("resume should fail when worktree preparation fails"),
            Err(error) => error,
        };

        match error {
            ExternalAgentError::ResumeUnavailable { session, detail } => {
                assert_eq!(session.session_id.as_deref(), Some("persisted-1"));
                assert!(detail.contains("worktree"), "detail names the cause");
            }
            other => panic!("expected ResumeUnavailable, got {other:?}"),
        }
        assert_eq!(resumes.load(Ordering::SeqCst), 0, "adapter never ran");
    }

    #[tokio::test]
    async fn external_runtime_registry_failed_start_discards_prepared_worktree() {
        // When the adapter refuses the start after preparation succeeded, the
        // prepared tree is discarded with a Graceful disposition so an ephemeral
        // worktree is not leaked for a session that never ran.
        let mut mock = MockAdapter::new(false);
        mock.fail_start = true;
        let adapter = Arc::new(mock);
        let stub = StubWorktreeManager::new();
        let registry = registry_with_stub(adapter, Arc::clone(&stub));
        let ctx = run_context();

        let result = registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await;
        assert!(result.is_err(), "the adapter's launch failure surfaces");
        assert_eq!(stub.prepares().len(), 1);
        assert_eq!(
            stub.cleanups(),
            vec![(
                StubWorktreeManager::prepared_path(0),
                ExternalSessionShutdown::Graceful,
            ),],
            "the prepared worktree was discarded after the failed start"
        );
        assert_eq!(registry.live_len(), 0);
    }

    #[tokio::test]
    async fn external_runtime_registry_cleanup_sweeps_worktree_with_session_disposition() {
        // M2-7: cleanup hands the session's recorded PreparedWorktree to the
        // WorktreeManager with the session's own shutdown disposition, so a
        // force-killed session keeps its ephemeral tree for inspection.
        let mut mock = MockAdapter::new(false);
        mock.shutdown_disposition = ExternalSessionShutdown::ForcedKill;
        let adapter = Arc::new(mock);
        let stub = StubWorktreeManager::new();
        let registry = registry_with_stub(adapter, Arc::clone(&stub));
        let ctx = run_context();

        let started = registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("start succeeds");
        let session_id = started.lock().await.session_ref().session_id.expect("id");

        let disposition = registry
            .cleanup(agent_id(), &session_ref(&session_id))
            .await;
        assert_eq!(disposition, ExternalSessionShutdown::ForcedKill);
        assert_eq!(
            stub.cleanups(),
            vec![(
                StubWorktreeManager::prepared_path(0),
                ExternalSessionShutdown::ForcedKill,
            ),],
            "the worktree manager saw the session's ForcedKill disposition"
        );
    }

    #[tokio::test]
    async fn external_runtime_registry_cleanup_agent_sweeps_each_prepared_worktree() {
        // The cancel sweep cleans up every swept session's prepared worktree
        // with that session's disposition.
        let adapter = Arc::new(MockAdapter::new(false));
        let stub = StubWorktreeManager::new();
        let registry = registry_with_stub(adapter, Arc::clone(&stub));
        let ctx = run_context();

        registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("first start");
        registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("second start");

        let dispositions = registry.cleanup_agent(agent_id()).await;
        assert_eq!(dispositions.len(), 2);
        assert_eq!(
            stub.cleanups(),
            vec![
                (
                    StubWorktreeManager::prepared_path(0),
                    ExternalSessionShutdown::Graceful,
                ),
                (
                    StubWorktreeManager::prepared_path(1),
                    ExternalSessionShutdown::Graceful,
                ),
            ],
            "each swept session's prepared worktree was cleaned up"
        );
    }

    #[tokio::test]
    async fn external_runtime_registry_worktree_cleanup_failure_escalates_disposition() {
        // A worktree that cannot be cleaned up may have been left behind, which
        // is a residual side effect: the returned disposition escalates to
        // Failed even though the session itself closed gracefully.
        let adapter = Arc::new(MockAdapter::new(false));
        let stub = StubWorktreeManager::new();
        let registry = registry_with_stub(adapter, Arc::clone(&stub));
        let ctx = run_context();

        let started = registry
            .get_or_start(&start_request(agent_id()), &ctx, None)
            .await
            .expect("start succeeds");
        let session_id = started.lock().await.session_ref().session_id.expect("id");

        stub.fail_cleanup.store(true, Ordering::SeqCst);
        let disposition = registry
            .cleanup(agent_id(), &session_ref(&session_id))
            .await;
        assert_eq!(
            disposition,
            ExternalSessionShutdown::Failed,
            "a failed worktree cleanup escalates the disposition"
        );
    }
}
