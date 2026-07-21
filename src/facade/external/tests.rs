use super::{
    CapabilitySource, ExternalAgentCapabilities, ExternalRunMode, ManagedExternalAgent,
    declared_capabilities,
};
use crate::agent::{ExternalCapability, ExternalPermissionMode, ExternalRuntimeKind};
use crate::facade::error::FacadeError;

#[test]
fn run_mode_labels_match_serde() {
    for mode in ExternalRunMode::ALL {
        let json = serde_json::to_value(mode).expect("serialize mode");
        assert_eq!(json, serde_json::Value::String(mode.as_str().to_owned()));
        assert_eq!(mode.to_string(), mode.as_str());
    }
}

#[test]
fn cli_presets_carry_expected_runtime_and_defaults() {
    let claude = ManagedExternalAgent::claude_code()
        .build()
        .expect("build claude");
    assert_eq!(claude.runtime(), &ExternalRuntimeKind::ClaudeCode);
    assert_eq!(claude.mode(), ExternalRunMode::Managed);
    assert_eq!(claude.permission_mode(), ExternalPermissionMode::Prompt);
    assert!(claude.worktree().is_none());
    assert!(claude.binary().is_none());
    // Claude Code declares a permission bridge; Codex/OpenCode do not.
    assert!(
        claude
            .capabilities()
            .supports(ExternalCapability::PermissionBridge)
    );

    let codex = ManagedExternalAgent::codex().build().expect("build codex");
    assert_eq!(codex.runtime(), &ExternalRuntimeKind::Codex);
    assert!(
        !codex
            .capabilities()
            .supports(ExternalCapability::PermissionBridge)
    );

    let opencode = ManagedExternalAgent::opencode()
        .build()
        .expect("build opencode");
    assert_eq!(opencode.runtime(), &ExternalRuntimeKind::OpenCode);
    assert!(
        !opencode
            .capabilities()
            .supports(ExternalCapability::PermissionBridge)
    );
}

#[test]
fn builder_records_launch_data() {
    let codex = ManagedExternalAgent::codex()
        .worktree("/tmp/repo")
        .model("gpt-5-mini")
        .arg("--foo")
        .permission_mode(ExternalPermissionMode::AcceptEdits)
        .mode(ExternalRunMode::Attachable)
        .build()
        .expect("build codex");

    assert_eq!(
        codex.worktree().map(|w| w.path().to_path_buf()),
        Some("/tmp/repo".into())
    );
    assert_eq!(codex.model(), Some("gpt-5-mini"));
    assert_eq!(codex.args(), ["--foo"]);
    assert_eq!(codex.permission_mode(), ExternalPermissionMode::AcceptEdits);
    // Attachable needs streaming + resume, both declared by Codex.
    assert_eq!(codex.mode(), ExternalRunMode::Attachable);
}

#[test]
fn args_replaces_full_list() {
    let codex = ManagedExternalAgent::codex()
        .arg("--first")
        .args(["--a", "--b"])
        .build()
        .expect("build codex");
    assert_eq!(codex.args(), ["--a", "--b"]);
}

#[test]
fn unsupported_mode_fails_fast_with_missing_capabilities() {
    // No current runtime injects host tools, so ManagedWithTools fails fast.
    let error = ManagedExternalAgent::codex()
        .mode(ExternalRunMode::ManagedWithTools)
        .build()
        .expect_err("host-tool grade must be rejected");

    match error {
        FacadeError::UnsupportedExternalMode {
            runtime,
            mode,
            missing,
            capability_source,
        } => {
            assert_eq!(runtime, "codex");
            assert_eq!(mode, "managed_with_tools");
            assert_eq!(missing, "host_tools");
            // The check was made against the preset's declared baseline.
            assert_eq!(capability_source, "declared");
        }
        other => panic!("expected UnsupportedExternalMode, got {other:?}"),
    }
}

#[test]
fn preset_capabilities_are_declared() {
    // A preset seeds the runtime's conservative *declared* baseline, not a
    // verified grade.
    let codex = ManagedExternalAgent::codex().build().expect("build codex");
    assert_eq!(codex.capabilities().source(), CapabilitySource::Declared);
}

#[test]
fn from_runtime_capabilities_is_supplied() {
    // The generic public wrapper records caller-supplied provenance so a
    // manual `.capabilities(..)` path is not conflated with a declared or
    // probed grade.
    let caps = ExternalAgentCapabilities::from_runtime_capabilities(declared_capabilities(
        &ExternalRuntimeKind::Codex,
    ));
    assert_eq!(caps.source(), CapabilitySource::Supplied);
    assert_eq!(
        ExternalAgentCapabilities::supplied(declared_capabilities(&ExternalRuntimeKind::Codex))
            .source(),
        CapabilitySource::Supplied
    );
}

#[test]
fn supplied_capabilities_flow_through_builder() {
    // Folding a caller-built capability set through `.capabilities(..)`
    // preserves its `Supplied` provenance on the built agent.
    let supplied =
        ExternalAgentCapabilities::supplied(declared_capabilities(&ExternalRuntimeKind::Codex));
    let codex = ManagedExternalAgent::codex()
        .capabilities(supplied)
        .build()
        .expect("build codex");
    assert_eq!(codex.capabilities().source(), CapabilitySource::Supplied);
}

#[test]
fn probed_capabilities_are_probed() {
    // The probe-provenance constructor tags its view accordingly; the default
    // handler builder (M4-4) folds such a view in after a real probe.
    let caps =
        ExternalAgentCapabilities::probed(declared_capabilities(&ExternalRuntimeKind::ClaudeCode));
    assert_eq!(caps.source(), CapabilitySource::Probed);
}

#[test]
fn capability_source_labels_match_serde() {
    for source in [
        CapabilitySource::Declared,
        CapabilitySource::Supplied,
        CapabilitySource::Probed,
        CapabilitySource::Negotiated,
    ] {
        let json = serde_json::to_value(source).expect("serialize source");
        assert_eq!(json, serde_json::Value::String(source.as_str().to_owned()));
        assert_eq!(source.to_string(), source.as_str());
    }
    assert_eq!(CapabilitySource::default(), CapabilitySource::Declared);
}

#[test]
fn capabilities_source_defaults_when_absent_from_serde() {
    // A view decoded from data that predates the source model falls back to
    // the conservative `Declared` baseline rather than failing.
    let mut encoded = serde_json::to_value(
        ManagedExternalAgent::codex()
            .build()
            .expect("build codex")
            .capabilities(),
    )
    .expect("serialize caps");
    encoded
        .as_object_mut()
        .expect("caps object")
        .remove("source");
    let decoded: ExternalAgentCapabilities =
        serde_json::from_value(encoded).expect("deserialize legacy caps");
    assert_eq!(decoded.source(), CapabilitySource::Declared);
}

#[test]
fn black_box_is_always_supported() {
    // Even a bare custom runtime with no declared capabilities serves BlackBox.
    let caps = ExternalAgentCapabilities::from_runtime_capabilities(declared_capabilities(
        &ExternalRuntimeKind::Custom("bespoke".to_owned()),
    ));
    assert!(caps.supports_mode(ExternalRunMode::BlackBox));
    assert!(!caps.supports_mode(ExternalRunMode::Managed));
    assert_eq!(caps.supported_modes(), vec![ExternalRunMode::BlackBox]);
}

#[test]
fn supported_modes_reflect_declared_capabilities() {
    // Codex: streaming + resume but no host tools → BlackBox/Managed/Attachable.
    let codex = ManagedExternalAgent::codex().build().expect("build codex");
    assert_eq!(
        codex.capabilities().supported_modes(),
        vec![
            ExternalRunMode::BlackBox,
            ExternalRunMode::Managed,
            ExternalRunMode::Attachable,
        ]
    );
    assert!(
        !codex
            .capabilities()
            .supports_mode(ExternalRunMode::ManagedWithTools)
    );
    assert_eq!(
        codex
            .capabilities()
            .missing_for_mode(ExternalRunMode::ManagedWithTools),
        vec![ExternalCapability::HostTools]
    );
}

#[test]
fn capabilities_view_roundtrips_and_exposes_inner() {
    let codex = ManagedExternalAgent::codex().build().expect("build codex");
    let caps = codex.capabilities();
    assert_eq!(caps.runtime(), &ExternalRuntimeKind::Codex);
    assert_eq!(
        caps.as_runtime_capabilities().runtime,
        ExternalRuntimeKind::Codex
    );
    let encoded = serde_json::to_value(caps).expect("serialize caps");
    let decoded: ExternalAgentCapabilities =
        serde_json::from_value(encoded).expect("deserialize caps");
    assert_eq!(&decoded, caps);
}

#[cfg(feature = "external-acp")]
#[test]
fn acp_presets_map_negotiated_capabilities() {
    use crate::agent::external::{AcpNegotiatedCapabilities, acp_runtime_kind};

    // Pre-negotiation baseline: streaming + permission bridge + graceful
    // shutdown, but resume is off, so Attachable is not yet available.
    let base = ManagedExternalAgent::opencode_acp()
        .build()
        .expect("build acp");
    assert_eq!(base.runtime(), &acp_runtime_kind());
    assert!(
        base.capabilities()
            .supports(ExternalCapability::PermissionBridge)
    );
    assert!(!base.capabilities().supports(ExternalCapability::Resume));
    assert!(
        !base
            .capabilities()
            .supports_mode(ExternalRunMode::Attachable)
    );
    // The pre-negotiation baseline is a static declared floor, not a live
    // handshake result.
    assert_eq!(base.capabilities().source(), CapabilitySource::Declared);

    // Attachable fails fast before load_session is negotiated.
    let error = ManagedExternalAgent::opencode_acp()
        .mode(ExternalRunMode::Attachable)
        .build()
        .expect_err("resume must be negotiated first");
    assert!(matches!(
        error,
        FacadeError::UnsupportedExternalMode {
            capability_source: "declared",
            ..
        }
    ));

    // Folding in a handshake that advertised session/load enables resume and
    // therefore the Attachable grade.
    let negotiated = AcpNegotiatedCapabilities::none().with_load_session(true);
    let attachable = ManagedExternalAgent::opencode_acp()
        .acp_negotiated(&negotiated)
        .mode(ExternalRunMode::Attachable)
        .build()
        .expect("resume available after negotiation");
    assert!(
        attachable
            .capabilities()
            .supports(ExternalCapability::Resume)
    );
    // A real negotiation result is tagged as such.
    assert_eq!(
        attachable.capabilities().source(),
        CapabilitySource::Negotiated
    );
    assert_eq!(attachable.mode(), ExternalRunMode::Attachable);
}

#[tokio::test]
async fn drive_external_marks_cleanup_on_cancel() {
    use super::drive_external;
    use crate::agent::{
        BudgetLimits, ExternalSessionHandler, ExternalSessionRequest, RequirementResult, RunContext,
    };
    use crate::facade::collab::CollabBridge;
    use crate::facade::ids::FacadeIds;
    use async_trait::async_trait;
    use std::sync::Arc;

    // A handler that must never be invoked: a pre-cancelled drive abandons the
    // session's opening `NeedExternalSession` before reaching `fulfill`.
    struct NeverInvokedHandler;

    #[async_trait]
    impl ExternalSessionHandler for NeverInvokedHandler {
        async fn fulfill(
            &self,
            _request: &ExternalSessionRequest,
            _ctx: &RunContext,
        ) -> RequirementResult {
            panic!("the session handler must not run when the drive is cancelled");
        }
    }

    let coder = ManagedExternalAgent::claude_code()
        .session_handler(Arc::new(NeverInvokedHandler))
        .build()
        .expect("managed external agent builds");

    let ids = FacadeIds::seeded(7);
    let ctx = RunContext::new_root(
        ids.run_id(),
        BudgetLimits::unbounded(),
        ids.trace_root("external-cancel"),
    );
    ctx.cancellation().cancel();

    let outcome = drive_external(
        "coder",
        &coder,
        &ids,
        "refactor".to_owned(),
        &CollabBridge::default(),
        None,
        &ctx,
    )
    .await
    .expect("a cancelled drive still returns its captured outcome");

    // The abandoned session left a cleanup marker for the handle layer to
    // sweep, and never reached a completed state (design §6.4).
    assert!(
        outcome.cleanup_required,
        "a cancelled external session leaves a cleanup marker"
    );
    assert!(
        !outcome.completed,
        "a cancelled external session did not complete"
    );
    assert!(outcome.artifacts.is_empty());
}

#[cfg(feature = "external-acp")]
fn acp_permission_interaction(
    ids: &crate::facade::ids::FacadeIds,
) -> (crate::agent::Interaction, crate::agent::AgentId) {
    use crate::agent::{PermissionCategory, PermissionRequest, PermissionRisk};

    let actor = ids.agent_id();
    let request = PermissionRequest::new(
        "act-1".to_owned(),
        actor,
        PermissionCategory::Shell,
        "run `cargo test`".to_owned(),
        serde_json::Value::Null,
        PermissionRisk::Medium,
        Some("verify the refactor".to_owned()),
    );
    (
        crate::agent::Interaction::permission(ids.step_id(), request),
        actor,
    )
}

#[cfg(feature = "external-acp")]
fn acp_completed_output(summary: &str) -> crate::agent::external::ExternalAgentOutput {
    crate::agent::external::ExternalAgentOutput {
        summary: summary.to_owned(),
        artifacts: Vec::new(),
        usage: None,
        cost_micros: None,
    }
}

#[cfg(feature = "external-acp")]
fn external_root_context(ids: &crate::facade::ids::FacadeIds) -> crate::agent::RunContext {
    crate::agent::RunContext::new_root(
        ids.run_id(),
        crate::agent::BudgetLimits::unbounded(),
        ids.trace_root("external-interaction-route"),
    )
}

#[cfg(feature = "external-acp")]
fn acp_session_ref() -> crate::agent::external::ExternalSessionRef {
    crate::agent::external::ExternalSessionRef {
        runtime: crate::agent::external::acp_runtime_kind(),
        session_id: Some("sess-1".to_owned()),
        transcript_ref: None,
        resume_token: None,
        last_event_seq: None,
    }
}

#[cfg(feature = "external-acp")]
fn acp_permission_pause(
    interaction: crate::agent::Interaction,
) -> crate::agent::external::ExternalSessionResult {
    use crate::agent::external::{ExternalAgentEvent, ExternalObservedEvent};

    crate::agent::external::ExternalSessionResult::PausedForInteraction {
        session: acp_session_ref(),
        action_id: "act-1".to_owned(),
        request: interaction,
        observations: ExternalObservedEvent::unsequenced_for_tests(vec![
            ExternalAgentEvent::PermissionRequested {
                action_id: "act-1".to_owned(),
                summary: "run `cargo test`".to_owned(),
            },
        ]),
    }
}

#[cfg(feature = "external-acp")]
fn acp_completed(summary: &str) -> crate::agent::external::ExternalSessionResult {
    crate::agent::external::ExternalSessionResult::Completed {
        session: acp_session_ref(),
        output: acp_completed_output(summary),
        observations: Vec::new(),
    }
}

#[cfg(feature = "external-acp")]
struct ScriptedExternalHandler {
    steps:
        std::sync::Mutex<std::collections::VecDeque<crate::agent::external::ExternalSessionResult>>,
    requests: std::sync::Arc<std::sync::Mutex<Vec<crate::agent::external::ExternalSessionRequest>>>,
}

#[cfg(feature = "external-acp")]
impl ScriptedExternalHandler {
    fn new(steps: impl IntoIterator<Item = crate::agent::external::ExternalSessionResult>) -> Self {
        Self {
            steps: std::sync::Mutex::new(steps.into_iter().collect()),
            requests: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn log(
        &self,
    ) -> std::sync::Arc<std::sync::Mutex<Vec<crate::agent::external::ExternalSessionRequest>>> {
        self.requests.clone()
    }
}

#[cfg(feature = "external-acp")]
#[async_trait::async_trait]
impl crate::agent::ExternalSessionHandler for ScriptedExternalHandler {
    async fn fulfill(
        &self,
        request: &crate::agent::external::ExternalSessionRequest,
        _ctx: &crate::agent::RunContext,
    ) -> crate::agent::RequirementResult {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(request.clone());
        let result = self
            .steps
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pop_front()
            .unwrap_or_else(|| crate::agent::external::ExternalSessionResult::Failed {
                session: None,
                error: crate::agent::external::ExternalAgentError::Runtime {
                    code: None,
                    message: "scripted external handler exhausted".to_owned(),
                    runtime_output: None,
                },
                observations: Vec::new(),
            });
        crate::agent::RequirementResult::ExternalSession(Box::new(result))
    }
}

#[cfg(feature = "external-acp")]
struct RecordingParentInteractionHandler {
    requests: std::sync::Arc<std::sync::Mutex<Vec<crate::agent::Interaction>>>,
}

#[cfg(feature = "external-acp")]
impl RecordingParentInteractionHandler {
    fn new() -> Self {
        Self {
            requests: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn log(&self) -> std::sync::Arc<std::sync::Mutex<Vec<crate::agent::Interaction>>> {
        self.requests.clone()
    }
}

#[cfg(feature = "external-acp")]
#[async_trait::async_trait]
impl crate::agent::InteractionHandler for RecordingParentInteractionHandler {
    async fn fulfill(
        &self,
        request: &crate::agent::Interaction,
        _ctx: &crate::agent::RunContext,
    ) -> crate::agent::RequirementResult {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(request.clone());
        let crate::agent::InteractionKind::Permission { request } = request.kind() else {
            panic!("expected permission interaction, got {:?}", request.kind());
        };
        crate::agent::RequirementResult::Interaction(crate::agent::InteractionResponse::Permission(
            crate::agent::PermissionResponse::approve(request.action_id().to_owned()),
        ))
    }
}

/// M3-R: a parent handler that parks forever, proving the external
/// interaction route abandons a parked interaction when the run is
/// cancelled (mirrors `ParkingParentInteractionHandler` in
/// `facade::delegate` for the local path).
#[cfg(feature = "external-acp")]
struct ParkingParentInteractionHandler {
    reached: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<crate::agent::Interaction>>>,
}

#[cfg(feature = "external-acp")]
impl ParkingParentInteractionHandler {
    fn new() -> (
        std::sync::Arc<Self>,
        tokio::sync::oneshot::Receiver<crate::agent::Interaction>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            std::sync::Arc::new(Self {
                reached: std::sync::Mutex::new(Some(tx)),
            }),
            rx,
        )
    }
}

#[cfg(feature = "external-acp")]
#[async_trait::async_trait]
impl crate::agent::InteractionHandler for ParkingParentInteractionHandler {
    async fn fulfill(
        &self,
        request: &crate::agent::Interaction,
        _ctx: &crate::agent::RunContext,
    ) -> crate::agent::RequirementResult {
        if let Some(sender) = self
            .reached
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            let _ = sender.send(request.clone());
        }
        std::future::pending::<crate::agent::RequirementResult>().await
    }
}

/// A capturing `AsyncWrite` recording every byte the ACP session writes.
#[cfg(feature = "external-acp")]
struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(feature = "external-acp")]
impl tokio::io::AsyncWrite for SharedWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// An async reader that serves scripted bytes and then pends forever,
/// modelling a live but silent ACP agent that never writes another line.
#[cfg(feature = "external-acp")]
struct ScriptedThenSilent {
    scripted: std::io::Cursor<Vec<u8>>,
}

#[cfg(feature = "external-acp")]
impl tokio::io::AsyncRead for ScriptedThenSilent {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        #[allow(clippy::cast_possible_truncation)]
        if self.scripted.position() < self.scripted.get_ref().len() as u64 {
            return std::pin::Pin::new(&mut self.scripted).poll_read(cx, buf);
        }
        std::task::Poll::Pending
    }
}

/// A fake ACP launcher whose agent answers the handshake from a script and
/// then stays silent forever, capturing every written frame.
#[cfg(feature = "external-acp")]
struct SilentTurnLauncher {
    handshake: std::sync::Mutex<Option<String>>,
    written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

#[cfg(feature = "external-acp")]
impl SilentTurnLauncher {
    fn new(lines: &[&str]) -> Self {
        // Every scripted line must be newline-terminated: the reader never
        // reports EOF, so an unterminated tail would pend forever.
        Self {
            handshake: std::sync::Mutex::new(Some(format!("{}\n", lines.join("\n")))),
            written: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn written(&self) -> String {
        String::from_utf8(
            self.written
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone(),
        )
        .expect("utf8 frames")
    }
}

#[cfg(feature = "external-acp")]
#[async_trait::async_trait]
impl crate::agent::external::AcpLauncher for SilentTurnLauncher {
    async fn launch(
        &self,
        _config: &crate::agent::external::AcpConfig,
    ) -> Result<crate::agent::external::SpawnedAcpAgent, crate::agent::external::ExternalAgentError>
    {
        let script = self
            .handshake
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .unwrap_or_default();
        let reader = ScriptedThenSilent {
            scripted: std::io::Cursor::new(script.into_bytes()),
        };
        let writer = SharedWriter(std::sync::Arc::clone(&self.written));
        // A read timeout far beyond the test's settle bound: settling fast
        // proves cancellation — not the IO timeout — ended the wait.
        Ok(crate::agent::external::SpawnedAcpAgent::new(
            writer,
            reader,
            std::time::Duration::from_secs(60),
        ))
    }
}

/// A worktree manager that hands out synthetic prepared paths and records
/// every cleanup call, so the test can watch the sweep's worktree wiring
/// without touching a real filesystem.
struct RecordingWorktreeManager {
    cleanups: std::sync::Mutex<
        Vec<(
            crate::agent::WorktreeRef,
            crate::agent::external::ExternalSessionShutdown,
        )>,
    >,
}

impl RecordingWorktreeManager {
    fn new() -> Self {
        Self {
            cleanups: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn cleanups(
        &self,
    ) -> Vec<(
        crate::agent::WorktreeRef,
        crate::agent::external::ExternalSessionShutdown,
    )> {
        self.cleanups
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

#[async_trait::async_trait]
impl crate::agent::external::WorktreeManager for RecordingWorktreeManager {
    async fn prepare(
        &self,
        agent_id: crate::agent::AgentId,
        base: &crate::agent::WorktreeRef,
        isolation: crate::agent::external::WorktreeIsolation,
    ) -> Result<crate::agent::external::PreparedWorktree, crate::agent::external::WorktreeError>
    {
        Ok(
            crate::agent::external::PreparedWorktree::new(agent_id, isolation, base.clone(), true)
                .with_base_repo(base.clone()),
        )
    }

    async fn cleanup(
        &self,
        prepared: crate::agent::external::PreparedWorktree,
        disposition: crate::agent::external::ExternalSessionShutdown,
    ) -> Result<crate::agent::external::WorktreeCleanupOutcome, crate::agent::external::WorktreeError>
    {
        self.cleanups
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push((prepared.worktree().clone(), disposition));
        Ok(crate::agent::external::WorktreeCleanupOutcome::new(
            prepared.isolation(),
            prepared.worktree().clone(),
            true,
            disposition.leaves_residual_side_effects(),
        ))
    }
}

/// Polls `condition` until it holds or `bound` elapses, returning whether
/// it held. The M3-R sweep runs as a detached background task, so tests
/// observe its effects through this instead of racing the spawned task.
async fn observed_within(bound: std::time::Duration, mut condition: impl FnMut() -> bool) -> bool {
    let start = std::time::Instant::now();
    while !condition() {
        if start.elapsed() >= bound {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    true
}

/// A session handler whose `cleanup_agent` waits `teardown` before
/// forwarding to the inner registry handler, emulating a slow-to-die
/// session — a child that lingers after stdin EOF while the adapter's
/// `shutdown_grace` waits on its exit.
#[cfg(feature = "external-acp")]
struct SlowToDieSessionHandler {
    inner: std::sync::Arc<crate::agent::external::RegistryExternalSessionHandler>,
    teardown: std::time::Duration,
}

#[cfg(feature = "external-acp")]
#[async_trait::async_trait]
impl crate::agent::ExternalSessionHandler for SlowToDieSessionHandler {
    async fn fulfill(
        &self,
        request: &crate::agent::external::ExternalSessionRequest,
        ctx: &crate::agent::RunContext,
    ) -> crate::agent::RequirementResult {
        self.inner.fulfill(request, ctx).await
    }

    async fn cleanup_agent(
        &self,
        agent_id: crate::agent::AgentId,
    ) -> Vec<crate::agent::external::ExternalSessionShutdown> {
        tokio::time::sleep(self.teardown).await;
        self.inner.cleanup_agent(agent_id).await
    }
}

/// M3-2: a cancelled facade drive force-closes the abandoned session
/// through the handler's registry with no host involvement — the runtime
/// observes `session/cancel`, the live handle is deregistered (no dangling
/// handle, no leaked subprocess), and the ephemeral worktree is swept with
/// the session's shutdown disposition.
#[cfg(feature = "external-acp")]
#[tokio::test]
async fn drive_external_cancel_sweeps_live_session_and_worktree() {
    use super::drive_external;
    use crate::agent::external::{
        AcpAdapter, AcpLauncher, ExternalRuntimeAdapter, ExternalSessionRegistry,
        ExternalSessionShutdown, WorktreeManager,
    };
    use crate::agent::{BudgetLimits, RunContext, TraceNodeKind};
    use crate::facade::collab::CollabBridge;
    use crate::facade::ids::FacadeIds;
    use std::sync::Arc;
    use std::time::Duration;

    let launcher = Arc::new(SilentTurnLauncher::new(&[
        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}"#,
    ]));
    let adapter = AcpAdapter::with_launcher(
        crate::agent::external::AcpConfig::opencode_acp(),
        Arc::clone(&launcher) as Arc<dyn AcpLauncher>,
    );
    let worktrees = Arc::new(RecordingWorktreeManager::new());
    let registry = Arc::new(ExternalSessionRegistry::with_worktree_manager(
        Arc::new(adapter) as Arc<dyn ExternalRuntimeAdapter>,
        Arc::clone(&worktrees) as Arc<dyn WorktreeManager>,
    ));
    let session_handler = Arc::new(crate::agent::external::RegistryExternalSessionHandler::new(
        Arc::clone(&registry),
    ));

    let agent = ManagedExternalAgent::opencode_acp()
        .session_handler(session_handler)
        .build()
        .expect("managed ACP external agent builds");

    let ids = FacadeIds::seeded(13);
    let ctx = RunContext::new_root(
        ids.run_id(),
        BudgetLimits::unbounded(),
        ids.trace_root("external-cancel-sweep"),
    );
    let token = ctx.cancellation().clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        drive_external(
            "coder",
            &agent,
            &ids,
            "refactor".to_owned(),
            &CollabBridge::default(),
            None,
            &ctx,
        ),
    )
    .await
    .expect("a cancelled drive settles in seconds, not after the read timeout")
    .expect("a cancelled drive still returns its captured outcome");
    canceller.await.expect("canceller task");

    assert!(
        outcome.cleanup_required,
        "a cancelled external session leaves a cleanup marker"
    );
    assert!(!outcome.completed);

    // M3-R (F1): the sweep runs as a detached background task, so poll for
    // its effects rather than assuming they landed before the drive
    // returned (this fake closes fast, so they land almost immediately):
    // the adapter's shutdown sent session/cancel and closed the
    // transport, the registry deregistered the live handle, the ephemeral
    // worktree was swept, and the disposition hit the run trace.
    let swept = observed_within(Duration::from_secs(5), || {
        launcher.written().contains(r#""method":"session/cancel""#)
            && registry.live_len() == 0
            && worktrees.cleanups().len() == 1
            && ctx
                .trace()
                .records()
                .iter()
                .any(|record| matches!(record.kind(), TraceNodeKind::ExternalShutdown { .. }))
    })
    .await;
    assert!(
        swept,
        "the detached sweep force-closed the abandoned session: {}",
        launcher.written()
    );

    // The ephemeral worktree was swept exactly once with the session's
    // shutdown disposition (the childless stand-in closes gracefully).
    let cleanups = worktrees.cleanups();
    assert_eq!(cleanups.len(), 1, "one swept session, one worktree cleanup");
    assert_eq!(cleanups[0].1, ExternalSessionShutdown::Graceful);

    // The sweep's disposition was audited into the run trace.
    let shutdown_nodes: Vec<_> = ctx
        .trace()
        .records()
        .into_iter()
        .filter(|record| matches!(record.kind(), TraceNodeKind::ExternalShutdown { .. }))
        .collect();
    assert_eq!(
        shutdown_nodes.len(),
        1,
        "the sweep is recorded in the trace"
    );

    // M3-R (F2): the audit node id carries the drive's agent id after the
    // run id, so two uncommitted drives swept under one outer run mint
    // distinct node ids instead of silently swallowing the second audit.
    let node_id = shutdown_nodes[0].id().to_string();
    assert!(
        node_id.starts_with(&format!("external-cleanup-sweep/{}/", ctx.run_id()))
            && node_id.matches('/').count() == 3,
        "the sweep node id is scoped by run and agent: {node_id}"
    );
}

/// M3-R (F1): with a slow-to-die session (~3s teardown, far beyond the
/// outer run's 2s `CANCEL_UNWIND_GRACE`), a cancelled drive still settles
/// promptly — the sweep is spawned as a detached task — and classified
/// teardown completes in the background: `session/cancel` reaches the live
/// runtime, the handle is deregistered, the worktree is swept, and the
/// disposition is audited into the shared run trace.
#[cfg(feature = "external-acp")]
#[tokio::test]
async fn drive_external_cancel_detached_sweep_outlives_unwind_grace() {
    use super::drive_external;
    use crate::agent::external::{
        AcpAdapter, AcpLauncher, ExternalRuntimeAdapter, ExternalSessionRegistry,
        RegistryExternalSessionHandler, WorktreeManager,
    };
    use crate::agent::{BudgetLimits, RunContext, TraceNodeKind};
    use crate::facade::collab::CollabBridge;
    use crate::facade::ids::FacadeIds;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let launcher = Arc::new(SilentTurnLauncher::new(&[
        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}"#,
    ]));
    let adapter = AcpAdapter::with_launcher(
        crate::agent::external::AcpConfig::opencode_acp(),
        Arc::clone(&launcher) as Arc<dyn AcpLauncher>,
    );
    let worktrees = Arc::new(RecordingWorktreeManager::new());
    let registry = Arc::new(ExternalSessionRegistry::with_worktree_manager(
        Arc::new(adapter) as Arc<dyn ExternalRuntimeAdapter>,
        Arc::clone(&worktrees) as Arc<dyn WorktreeManager>,
    ));
    // Teardown takes ~3s: beyond the outer run's 2s CANCEL_UNWIND_GRACE,
    // so an inline sweep would be dropped mid-flight by a cancelled outer
    // run (the F1 finding).
    let session_handler = Arc::new(SlowToDieSessionHandler {
        inner: Arc::new(RegistryExternalSessionHandler::new(Arc::clone(&registry))),
        teardown: Duration::from_secs(3),
    });

    let agent = ManagedExternalAgent::opencode_acp()
        .session_handler(session_handler)
        .build()
        .expect("managed ACP external agent builds");

    let ids = FacadeIds::seeded(15);
    let ctx = RunContext::new_root(
        ids.run_id(),
        BudgetLimits::unbounded(),
        ids.trace_root("external-cancel-detached-sweep"),
    );
    let token = ctx.cancellation().clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });

    let started = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        drive_external(
            "coder",
            &agent,
            &ids,
            "refactor".to_owned(),
            &CollabBridge::default(),
            None,
            &ctx,
        ),
    )
    .await
    .expect("a cancelled drive settles in seconds, not after the read timeout")
    .expect("a cancelled drive still returns its captured outcome");
    canceller.await.expect("canceller task");

    // The drive returned after spawning the sweep, not after awaiting the
    // ~3s teardown: well inside the 2s unwind grace an outer run allows.
    let settled = started.elapsed();
    assert!(
        settled < Duration::from_secs(2),
        "the drive settles promptly, detaching the slow sweep: {settled:?}"
    );
    assert!(outcome.cleanup_required);
    assert!(!outcome.completed);

    // The detached sweep still runs classified teardown to completion in
    // the background, beyond any outer-run unwind grace.
    let swept = observed_within(Duration::from_secs(10), || {
        launcher.written().contains(r#""method":"session/cancel""#)
            && registry.live_len() == 0
            && worktrees.cleanups().len() == 1
            && ctx
                .trace()
                .records()
                .iter()
                .any(|record| matches!(record.kind(), TraceNodeKind::ExternalShutdown { .. }))
    })
    .await;
    assert!(
        swept,
        "the detached sweep completed slow classified teardown: {}",
        launcher.written()
    );
}

#[cfg(feature = "external-acp")]
#[tokio::test]
async fn drive_external_routes_permission_interaction_to_parent_handler() {
    use super::drive_external;
    use crate::agent::{
        ExternalSessionInput, InteractionHandler, InteractionKind, InteractionResponse,
        PermissionDecision,
    };
    use crate::facade::collab::CollabBridge;
    use crate::facade::ids::FacadeIds;
    use std::sync::Arc;

    let ids = FacadeIds::seeded(11);
    let (interaction, actor) = acp_permission_interaction(&ids);
    let runtime = ScriptedExternalHandler::new([
        acp_permission_pause(interaction),
        acp_completed("external complete"),
    ]);
    let external_log = runtime.log();

    let parent = RecordingParentInteractionHandler::new();
    let parent_log = parent.log();
    let parent_handler: Arc<dyn InteractionHandler> = Arc::new(parent);

    let agent = ManagedExternalAgent::opencode_acp()
        .session_handler(Arc::new(runtime))
        .build()
        .expect("managed ACP external agent builds");
    let ctx = external_root_context(&ids);

    let outcome = drive_external(
        "coder",
        &agent,
        &ids,
        "refactor".to_owned(),
        &CollabBridge::default(),
        Some(parent_handler),
        &ctx,
    )
    .await
    .expect("parent interaction handler resolves the external permission prompt");

    assert!(outcome.completed);
    assert_eq!(outcome.summary, "external complete");

    let interaction_records = parent_log
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    assert_eq!(interaction_records.len(), 1);
    let request = &interaction_records[0];
    let origin = request
        .origin
        .as_deref()
        .expect("external route marks origin");
    assert_eq!(origin.delegate, "coder");
    assert_eq!(origin.depth, 1);
    match request.kind() {
        InteractionKind::Permission { request } => {
            assert_eq!(request.action_id(), "act-1");
            assert_eq!(request.actor(), actor);
        }
        other => panic!("expected permission interaction, got {other:?}"),
    }

    let external_records = external_log
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    assert_eq!(external_records.len(), 2);
    assert!(matches!(
        external_records[0].input,
        ExternalSessionInput::Start { .. }
    ));
    match &external_records[1].input {
        ExternalSessionInput::RespondInteraction {
            action_id,
            response,
        } => {
            assert_eq!(action_id, "act-1");
            match response {
                InteractionResponse::Permission(response) => {
                    assert_eq!(response.action_id(), "act-1");
                    assert_eq!(response.decision(), &PermissionDecision::Approve);
                }
                other => panic!("expected permission response, got {other:?}"),
            }
        }
        other => panic!("expected RespondInteraction, got {other:?}"),
    }
}

/// M3-R (C9): cancelling while the parent interaction handler is parked
/// forever must not deadlock the external delegation — the route selects
/// on the cancellation token, the drive settles in seconds, and the
/// abandoned session is marked for cleanup (mirrors the M1-2 local test
/// `cancelling_while_parent_child_interaction_handler_is_parked_does_not_hang`).
#[cfg(feature = "external-acp")]
#[tokio::test]
async fn drive_external_cancel_while_parent_handler_parked_does_not_hang() {
    use super::drive_external;
    use crate::agent::InteractionHandler;
    use crate::facade::collab::CollabBridge;
    use crate::facade::ids::FacadeIds;
    use std::sync::Arc;
    use std::time::Duration;

    let ids = FacadeIds::seeded(14);
    let (interaction, _actor) = acp_permission_interaction(&ids);
    let runtime = ScriptedExternalHandler::new([acp_permission_pause(interaction)]);
    let external_log = runtime.log();

    let (parent, reached_rx) = ParkingParentInteractionHandler::new();
    let parent_handler: Arc<dyn InteractionHandler> = parent;

    let agent = ManagedExternalAgent::opencode_acp()
        .session_handler(Arc::new(runtime))
        .build()
        .expect("managed ACP external agent builds");
    let ctx = external_root_context(&ids);
    let token = ctx.cancellation().clone();
    let bridge = CollabBridge::default();

    let drive = drive_external(
        "coder",
        &agent,
        &ids,
        "refactor".to_owned(),
        &bridge,
        Some(parent_handler),
        &ctx,
    );
    let canceller = async move {
        let interaction = tokio::time::timeout(Duration::from_secs(1), reached_rx)
            .await
            .expect("parent handler should be reached before the test timeout")
            .expect("parent handler sends the interaction");
        let origin = interaction
            .origin()
            .expect("parked external interaction carries delegate attribution");
        assert_eq!(origin.delegate, "coder");
        assert_eq!(origin.depth, 1);
        token.cancel();
    };

    // M3-R (F3): 5s, comfortably clear of the drive's own 2s
    // `CANCEL_UNWIND_GRACE`, so this test asserts "does not hang" rather
    // than racing the exact grace boundary.
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        let (outcome, ()) = tokio::join!(drive, canceller);
        outcome
    })
    .await
    .expect("cancelling a parked external interaction must not hang")
    .expect("a cancelled drive still returns its captured outcome");

    assert!(outcome.cleanup_required);
    assert!(!outcome.completed);

    // The cancellation won the route before any answer came back, so the
    // runtime only ever observed the session start — no RespondInteraction.
    let external_records = external_log
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    assert_eq!(external_records.len(), 1);
}

#[cfg(feature = "external-acp")]
#[tokio::test]
async fn drive_external_permission_without_parent_handler_fails_clearly() {
    use super::drive_external;
    use crate::facade::collab::CollabBridge;
    use crate::facade::ids::FacadeIds;
    use std::sync::Arc;

    let ids = FacadeIds::seeded(12);
    let (interaction, _actor) = acp_permission_interaction(&ids);
    let runtime = ScriptedExternalHandler::new([acp_permission_pause(interaction)]);
    let external_log = runtime.log();

    let agent = ManagedExternalAgent::opencode_acp()
        .session_handler(Arc::new(runtime))
        .build()
        .expect("managed ACP external agent builds");
    let ctx = external_root_context(&ids);

    let error = drive_external(
        "coder",
        &agent,
        &ids,
        "refactor".to_owned(),
        &CollabBridge::default(),
        None,
        &ctx,
    )
    .await
    .expect_err("permission prompt without parent handler must fail clearly");

    match error {
        FacadeError::ExternalAgent { name, message } => {
            assert_eq!(name, "coder");
            assert!(
                message.contains("external agent requested permission")
                    && message.contains("no interaction handler"),
                "unexpected error message: {message}"
            );
        }
        other => panic!("expected ExternalAgent error, got {other:?}"),
    }

    let external_records = external_log
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    assert_eq!(external_records.len(), 1);
}

#[cfg(feature = "external-acp")]
#[test]
fn acp_arbitrary_launch_line_records_binary_and_args() {
    let agent = ManagedExternalAgent::acp("gemini", ["--experimental-acp"])
        .build()
        .expect("build acp");
    assert_eq!(
        agent.binary().map(std::path::Path::to_path_buf),
        Some("gemini".into())
    );
    assert_eq!(agent.args(), ["--experimental-acp"]);
}

// A runtime whose adapter feature is not compiled into this build fails fast
// with an explicit "enable the feature" message rather than degrading
// silently. Codex is used because this arm only runs when its feature is off.
#[cfg(not(feature = "external-codex"))]
#[tokio::test]
async fn build_with_default_session_handler_fails_fast_when_feature_disabled() {
    let error = ManagedExternalAgent::codex()
        .build_with_default_session_handler()
        .await
        .expect_err("a runtime with no compiled adapter must fail fast");
    match error {
        FacadeError::ExternalAgent { name, message } => {
            assert_eq!(name, "codex");
            assert!(
                message.contains("external-codex"),
                "the message must name the feature to enable, got: {message}"
            );
            // The fail-fast message names only the feature to enable — never a
            // launch line, environment variable, or credential.
            assert!(
                !message.contains("KEY") && !message.contains("TOKEN"),
                "the fail-fast message must not leak a secret, got: {message}"
            );
        }
        other => panic!("expected a fail-fast ExternalAgent error, got {other:?}"),
    }
}

// A caller-supplied handler is honored verbatim by the one-call build path:
// it short-circuits the probe, so the manual/custom-handler path keeps working
// regardless of which `external-*` features are compiled in.
#[tokio::test]
async fn build_with_default_session_handler_honors_supplied_handler() {
    use crate::agent::{
        ExternalSessionHandler, ExternalSessionRequest, RequirementResult, RunContext,
    };
    use async_trait::async_trait;
    use std::sync::Arc;

    struct NeverInvokedHandler;

    #[async_trait]
    impl ExternalSessionHandler for NeverInvokedHandler {
        async fn fulfill(
            &self,
            _request: &ExternalSessionRequest,
            _ctx: &RunContext,
        ) -> RequirementResult {
            panic!("the supplied handler must not run during assembly");
        }
    }

    let agent = ManagedExternalAgent::codex()
        .session_handler(Arc::new(NeverInvokedHandler))
        .build_with_default_session_handler()
        .await
        .expect("a supplied handler short-circuits the default assembly");
    assert!(
        agent.session_handler().is_some(),
        "the supplied handler must flow through to the built agent"
    );
}

// The manual `.session_handler(..).build()` path stays usable on its own.
#[test]
fn manual_session_handler_path_still_builds() {
    use crate::agent::{
        ExternalSessionHandler, ExternalSessionRequest, RequirementResult, RunContext,
    };
    use async_trait::async_trait;
    use std::sync::Arc;

    struct NeverInvokedHandler;

    #[async_trait]
    impl ExternalSessionHandler for NeverInvokedHandler {
        async fn fulfill(
            &self,
            _request: &ExternalSessionRequest,
            _ctx: &RunContext,
        ) -> RequirementResult {
            unreachable!()
        }
    }

    let agent = ManagedExternalAgent::codex()
        .session_handler(Arc::new(NeverInvokedHandler))
        .build()
        .expect("the manual session-handler path still builds");
    assert!(agent.session_handler().is_some());
}

// A runtime whose adapter feature is not compiled into this build fails fast
// with an explicit "enable the feature" message rather than degrading
// silently. Codex is used because this arm only runs when its feature is off.
#[cfg(not(feature = "external-codex"))]
#[tokio::test]
async fn default_handler_fails_fast_when_runtime_feature_disabled() {
    use super::default_external_session_handler;

    let codex = ManagedExternalAgent::codex().build().expect("build codex");
    let error = default_external_session_handler(&codex)
        .await
        .expect_err("a runtime with no compiled adapter must fail fast");
    match error {
        FacadeError::ExternalAgent { name, message } => {
            assert_eq!(name, "codex");
            assert!(
                message.contains("external-codex"),
                "the message must name the feature to enable, got: {message}"
            );
        }
        other => panic!("expected a fail-fast ExternalAgent error, got {other:?}"),
    }
}

// When the adapter feature *is* compiled in, a missing/broken CLI binary makes
// the capability probe fail fast with a non-secret error rather than silently
// building a degraded handler. An absolute non-existent path guarantees the
// probe's spawn fails offline without touching PATH.
#[cfg(feature = "external-claude-code")]
#[tokio::test]
async fn default_handler_fails_fast_when_cli_binary_is_missing() {
    use super::default_external_session_handler;

    let claude = ManagedExternalAgent::claude_code()
        .binary("/nonexistent-agent-lib/claude-probe-target")
        .build()
        .expect("build claude");
    let error = default_external_session_handler(&claude)
        .await
        .expect_err("a missing CLI binary must make the probe fail fast");
    match error {
        FacadeError::ExternalAgent { name, message } => {
            assert_eq!(name, "claude_code");
            assert!(
                !message.is_empty(),
                "the fail-fast error must carry a non-empty, non-secret message"
            );
        }
        other => panic!("expected a fail-fast ExternalAgent error, got {other:?}"),
    }
}

// A probe can report a *narrower* grade than the declared baseline. Once a
// `Probed` view is folded in, the capability gate follows it — a capability
// the declared baseline advertises but the probe did not verify is rejected,
// and the classified error names the capability and provenance without a
// secret.
#[test]
fn require_capability_gates_against_probed_view() {
    // The Claude Code declared baseline advertises a permission bridge; model
    // a probe that verified streaming but not the permission bridge or host
    // tools.
    let mut probed_caps = declared_capabilities(&ExternalRuntimeKind::ClaudeCode);
    assert!(
        probed_caps.permission_bridge,
        "the Claude Code declared baseline advertises a permission bridge"
    );
    probed_caps.permission_bridge = false;
    probed_caps.host_tools = false;

    let agent = ManagedExternalAgent::claude_code()
        .mode(ExternalRunMode::BlackBox)
        .capabilities(ExternalAgentCapabilities::probed(probed_caps))
        .build()
        .expect("black-box needs no capability, so the build succeeds");

    // The agent holds the probed view, not the declared baseline.
    assert_eq!(agent.capabilities().source(), CapabilitySource::Probed);

    // A capability the probe verified passes the gate.
    agent
        .require_capability(ExternalCapability::Streaming)
        .expect("streaming was probed as supported");

    // A capability the declared baseline advertises but the probe did not is
    // rejected — the probed view wins over the declared one.
    let error = agent
        .require_capability(ExternalCapability::PermissionBridge)
        .expect_err("the permission bridge was not verified by the probe");
    match error {
        FacadeError::UnsupportedExternalCapability {
            runtime,
            capability,
            capability_source,
        } => {
            assert_eq!(runtime, "claude_code");
            assert_eq!(capability, "permission_bridge");
            assert_eq!(capability_source, "probed");
        }
        other => panic!("expected UnsupportedExternalCapability, got {other:?}"),
    }

    // The rendered error names the capability and provenance but never a
    // launch line, environment variable, or credential.
    let rendered = agent
        .require_capability(ExternalCapability::HostTools)
        .expect_err("host tools were not verified by the probe")
        .to_string();
    assert!(
        rendered.contains("host_tools"),
        "the error must name the capability, got: {rendered}"
    );
    assert!(
        rendered.contains("probed"),
        "the error must name the capability source, got: {rendered}"
    );
    assert!(
        !rendered.contains("KEY") && !rendered.contains("TOKEN"),
        "the capability error must not leak a secret, got: {rendered}"
    );
}

// A declared-baseline preset still reports the `declared` provenance in the
// gate error, so a host can tell a conservative baseline apart from a probed
// grade.
#[test]
fn require_capability_reports_declared_provenance_for_a_preset() {
    let codex = ManagedExternalAgent::codex().build().expect("build codex");
    // Codex's declared baseline honestly advertises streaming.
    codex
        .require_capability(ExternalCapability::Streaming)
        .expect("codex declares streaming");
    let error = codex
        .require_capability(ExternalCapability::HostTools)
        .expect_err("codex declares no host-tool bridge");
    match error {
        FacadeError::UnsupportedExternalCapability {
            runtime,
            capability,
            capability_source,
        } => {
            assert_eq!(runtime, "codex");
            assert_eq!(capability, "host_tools");
            assert_eq!(capability_source, "declared");
        }
        other => panic!("expected UnsupportedExternalCapability, got {other:?}"),
    }
}

// The capabilities-returning helper fails fast with the same non-secret
// "enable the feature" error when the runtime's adapter feature is off, so a
// host that wants the probed view never gets a silent no-op.
#[cfg(not(feature = "external-codex"))]
#[tokio::test]
async fn default_handler_with_capabilities_fails_fast_when_feature_disabled() {
    use super::default_external_session_handler_with_capabilities;

    let codex = ManagedExternalAgent::codex().build().expect("build codex");
    let error = default_external_session_handler_with_capabilities(&codex)
        .await
        .expect_err("a runtime with no compiled adapter must fail fast");
    match error {
        FacadeError::ExternalAgent { name, message } => {
            assert_eq!(name, "codex");
            assert!(
                message.contains("external-codex"),
                "the message must name the feature to enable, got: {message}"
            );
            assert!(
                !message.contains("KEY") && !message.contains("TOKEN"),
                "the fail-fast message must not leak a secret, got: {message}"
            );
        }
        other => panic!("expected a fail-fast ExternalAgent error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// M1-1 `run_external_once` one-shot tests — all offline and free of any
// `external-*` feature: a fake adapter/session behind a real registry stands
// in for the live runtime.
// ---------------------------------------------------------------------------

/// How a fake one-shot session answers `advance`.
#[derive(Clone)]
enum OnceAdvance {
    /// Complete the session immediately with this summary.
    Complete(&'static str),
    /// Pend forever, modelling a live but silent runtime a cancel abandons
    /// mid-turn.
    Hang,
}

/// A fake runtime adapter whose sessions follow a fixed [`OnceAdvance`]
/// script and whose `shutdown` calls are counted, so the one-shot tests can
/// observe the sweep without any `external-*` feature.
struct OnceFakeAdapter {
    advance: OnceAdvance,
    shutdowns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// The live session [`OnceFakeAdapter`] starts, keyed `once-sess-1` so the
/// registry can register it.
struct OnceFakeSession {
    advance: OnceAdvance,
    session: crate::agent::external::ExternalSessionRef,
    shutdowns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

fn once_session_ref() -> crate::agent::external::ExternalSessionRef {
    crate::agent::external::ExternalSessionRef {
        runtime: crate::agent::ExternalRuntimeKind::ClaudeCode,
        session_id: Some("once-sess-1".to_owned()),
        transcript_ref: None,
        resume_token: None,
        last_event_seq: None,
    }
}

#[async_trait::async_trait]
impl crate::agent::external::ExternalRuntimeAdapter for OnceFakeAdapter {
    fn kind(&self) -> crate::agent::ExternalRuntimeKind {
        crate::agent::ExternalRuntimeKind::ClaudeCode
    }

    fn capabilities(&self) -> crate::agent::ExternalRuntimeCapabilities {
        crate::agent::ExternalRuntimeCapabilities::none(self.kind())
    }

    async fn start(
        &self,
        _request: &crate::agent::external::ExternalSessionRequest,
        _ctx: &crate::agent::RunContext,
        _sink: Option<std::sync::Arc<dyn crate::agent::external::ExternalEventSink>>,
    ) -> Result<
        Box<dyn crate::agent::external::ExternalRuntimeSession>,
        crate::agent::external::ExternalAgentError,
    > {
        Ok(Box::new(OnceFakeSession {
            advance: self.advance.clone(),
            session: once_session_ref(),
            shutdowns: std::sync::Arc::clone(&self.shutdowns),
        }))
    }
}

#[async_trait::async_trait]
impl crate::agent::external::ExternalRuntimeSession for OnceFakeSession {
    fn session_ref(&self) -> crate::agent::external::ExternalSessionRef {
        self.session.clone()
    }

    async fn advance(
        &mut self,
        _input: &crate::agent::external::ExternalSessionInput,
        _ctx: &crate::agent::RunContext,
    ) -> Result<
        crate::agent::external::RuntimeDecisionPoint,
        crate::agent::external::ExternalAgentError,
    > {
        match &self.advance {
            OnceAdvance::Complete(summary) => {
                Ok(crate::agent::external::RuntimeDecisionPoint::Completed {
                    session: self.session.clone(),
                    output: crate::agent::external::ExternalAgentOutput {
                        summary: (*summary).to_owned(),
                        artifacts: Vec::new(),
                        usage: None,
                        cost_micros: None,
                    },
                    observations: Vec::new(),
                })
            }
            OnceAdvance::Hang => {
                std::future::pending::<
                    Result<
                        crate::agent::external::RuntimeDecisionPoint,
                        crate::agent::external::ExternalAgentError,
                    >,
                >()
                .await
            }
        }
    }

    async fn shutdown(&mut self) -> crate::agent::external::ExternalSessionShutdown {
        self.shutdowns
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        crate::agent::external::ExternalSessionShutdown::Graceful
    }
}

/// The one-shot test rig: a managed external agent whose registry-backed
/// handler drives the fake runtime, plus the observable pieces the tests
/// assert against.
struct OnceRig {
    agent: ManagedExternalAgent,
    registry: std::sync::Arc<crate::agent::external::ExternalSessionRegistry>,
    worktrees: std::sync::Arc<RecordingWorktreeManager>,
    shutdowns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

fn once_rig(advance: OnceAdvance) -> OnceRig {
    use crate::agent::external::{
        ExternalRuntimeAdapter, ExternalSessionRegistry, RegistryExternalSessionHandler,
        WorktreeManager,
    };
    use std::sync::Arc;

    let shutdowns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worktrees = Arc::new(RecordingWorktreeManager::new());
    let registry = Arc::new(ExternalSessionRegistry::with_worktree_manager(
        Arc::new(OnceFakeAdapter {
            advance,
            shutdowns: std::sync::Arc::clone(&shutdowns),
        }) as Arc<dyn ExternalRuntimeAdapter>,
        Arc::clone(&worktrees) as Arc<dyn WorktreeManager>,
    ));
    let agent = ManagedExternalAgent::claude_code()
        .session_handler(Arc::new(RegistryExternalSessionHandler::new(Arc::clone(
            &registry,
        ))))
        .build()
        .expect("managed external agent builds");
    OnceRig {
        agent,
        registry,
        worktrees,
        shutdowns,
    }
}

/// M1-1 happy path: the one-shot drive runs the task to completion and
/// returns the session's final summary as the outcome.
#[tokio::test]
async fn run_external_once_completes_and_returns_summary() {
    use super::run_external_once;
    use crate::agent::{BudgetLimits, CancellationToken};
    use crate::facade::ids::FacadeIds;

    let rig = once_rig(OnceAdvance::Complete("refactor done"));

    let outcome = run_external_once(
        "coder",
        &rig.agent,
        &FacadeIds::seeded(21),
        "refactor the module".to_owned(),
        None,
        BudgetLimits::unbounded(),
        CancellationToken::new(),
    )
    .await
    .expect("the one-shot drive completes");

    assert!(outcome.completed);
    assert_eq!(outcome.summary, "refactor done");
    assert!(!outcome.cleanup_required);
}

/// M1-1 reclamation: the delegation drive deliberately leaves a *committed*
/// session live for reuse; the one-shot wrapper schedules its own detached
/// sweep, so the registry drains and the session is shut down with its
/// worktree swept — all without host involvement.
#[tokio::test]
async fn run_external_once_completed_sweeps_the_live_session() {
    use super::run_external_once;
    use crate::agent::{BudgetLimits, CancellationToken};
    use crate::facade::ids::FacadeIds;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    let rig = once_rig(OnceAdvance::Complete("done"));

    let outcome = run_external_once(
        "coder",
        &rig.agent,
        &FacadeIds::seeded(22),
        "refactor".to_owned(),
        None,
        BudgetLimits::unbounded(),
        CancellationToken::new(),
    )
    .await
    .expect("the one-shot drive completes");
    assert!(outcome.completed);

    // The drive left the committed session live; the wrapper's sweep is a
    // detached task that cannot have run yet — the current-thread runtime had
    // no await point between the call and this assertion.
    assert_eq!(
        rig.registry.live_len(),
        1,
        "a committed drive keeps its live session until the wrapper's sweep lands"
    );

    let swept = observed_within(Duration::from_secs(5), || {
        rig.registry.live_len() == 0
            && rig.shutdowns.load(Ordering::SeqCst) == 1
            && rig.worktrees.cleanups().len() == 1
    })
    .await;
    assert!(
        swept,
        "the one-shot wrapper's detached sweep reclaimed the completed session"
    );
}

/// M1-1 cancel: a cancelled one-shot drive settles promptly with an
/// uncompleted, cleanup-marked outcome, and the drive's (unchanged)
/// uncommitted-outcome sweep reclaims the abandoned session.
#[tokio::test]
async fn run_external_once_cancel_abandons_and_sweeps_the_live_session() {
    use super::run_external_once;
    use crate::agent::{BudgetLimits, CancellationToken};
    use crate::facade::ids::FacadeIds;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    let rig = once_rig(OnceAdvance::Hang);

    let token = CancellationToken::new();
    let cancel_token = token.clone();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_token.cancel();
    });

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_external_once(
            "coder",
            &rig.agent,
            &FacadeIds::seeded(23),
            "refactor".to_owned(),
            None,
            BudgetLimits::unbounded(),
            token,
        ),
    )
    .await
    .expect("a cancelled one-shot drive settles in seconds, not after the silent runtime")
    .expect("a cancelled drive still returns its captured outcome");
    canceller.await.expect("canceller task");

    assert!(!outcome.completed, "a cancelled drive did not complete");
    assert!(
        outcome.cleanup_required,
        "the abandoned session is marked for cleanup"
    );

    let swept = observed_within(Duration::from_secs(5), || {
        rig.registry.live_len() == 0
            && rig.shutdowns.load(Ordering::SeqCst) == 1
            && rig.worktrees.cleanups().len() == 1
    })
    .await;
    assert!(
        swept,
        "the detached sweep force-closed the abandoned session"
    );
}

/// M1-1 fail-fast: a managed external agent with no runtime session handler
/// cannot be driven — the one-shot entry surfaces the same classified
/// `ExternalAgent` error the delegation drive returns.
#[tokio::test]
async fn run_external_once_without_session_handler_fails_fast() {
    use super::run_external_once;
    use crate::agent::{BudgetLimits, CancellationToken};
    use crate::facade::ids::FacadeIds;

    let agent = ManagedExternalAgent::claude_code()
        .build()
        .expect("the preset builds without a session handler");

    let error = run_external_once(
        "coder",
        &agent,
        &FacadeIds::seeded(24),
        "refactor".to_owned(),
        None,
        BudgetLimits::unbounded(),
        CancellationToken::new(),
    )
    .await
    .expect_err("a managed external agent with no session handler cannot be driven");

    match error {
        FacadeError::ExternalAgent { name, message } => {
            assert_eq!(name, "coder");
            assert!(
                message.contains("no runtime session handler"),
                "unexpected error message: {message}"
            );
        }
        other => panic!("expected ExternalAgent error, got {other:?}"),
    }
}
