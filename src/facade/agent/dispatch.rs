//! Per-run trace collection and the delegation drive machinery.
//!
//! Split out of `agent.rs`: [`collect_traces`] projects drained notifications
//! into [`RunOutput`] traces/events, and the rules-routed / dispatcher-routed
//! drives run delegate topologies outside the model-routed loop.

use std::collections::HashMap;
use std::sync::Arc;

use crate::agent::requirement::AgentSpecRef;
use crate::agent::{
    Capability, CostTier, EscalationError, EscalationOutcome, EscalationRules, EscalationTrigger,
    Escalator, HumanGate, ImpactScope, Notification, PermissionRisk, RequirementResult, RunContext,
    ScriptedVerifier, TaskDescriptor, TaskEvaluator, Uncertainty, Verifier, WorkerProfile,
    WorkerProfileRef, WorkerReport, WorkerRoster,
};
use crate::conversation::Conversation;
use crate::facade::delegate::{
    DISPATCHER_ESCALATE_MARKER, DelegationRecorder, DelegationToolHandler, DispatcherConfig,
    RecordedDelegation, RulesRoutedTarget, SharedTaskEvaluator, SharedVerifier,
};
use crate::facade::error::FacadeError;
use crate::facade::external::{ExternalDelegateStatus, RetainedExternalSession};
use crate::facade::ids::FacadeIds;
use crate::facade::run::{
    ArtifactRef, DelegationStatus, DelegationTrace, EscalationTrace, Reply, RunEvent, RunOutput,
    ToolTrace, UsageSummary,
};
use crate::model::content::ContentBlock;
use crate::model::message::Message;

/// The per-run traces and UI events projected from a drained turn.
pub(crate) struct CollectedTraces {
    /// Traces for ordinary (non-delegation) tool calls.
    pub tool_calls: Vec<ToolTrace>,
    /// Traces for delegation calls, recorded by the delegation handler.
    pub delegations: Vec<DelegationTrace>,
    /// Aggregate token usage reported by every driven local subagent.
    pub subagent_usage: crate::model::usage::Usage,
    /// Aggregate token usage reported by every driven managed external agent.
    pub external_usage: crate::model::usage::Usage,
    /// Artifacts (patches/diffs/files/test results) reported by external
    /// delegates, in the order their delegations completed.
    pub artifacts: Vec<ArtifactRef>,
    /// The ordered normalized events for the run.
    pub events: Vec<RunEvent>,
    /// Whether any managed external delegate was denied before it started by the
    /// approval policy (§9.2). The Agent facade folds this into a run-level
    /// [`FacadeError::ApprovalDenied`].
    pub external_approval_denied: bool,
    /// The last-known data-only session facts for each managed external delegate
    /// driven this run, keyed by delegate name, for snapshot retention (§15.2).
    pub external_sessions: HashMap<String, RetainedExternalSession>,
}

/// Projects the drained tool notifications into per-call traces and UI events,
/// splitting delegation calls out from ordinary tool calls.
///
/// A [`Notification::ToolCallStarted`] carries the tool name and framework call
/// id. When that call id was recorded as a delegation by the
/// [`DelegationToolHandler`], it seeds a [`DelegationTrace`] in `delegations`
/// (its child usage folded into `subagent_usage` for a local subagent or
/// `external_usage` for a managed external agent) and a
/// [`RunEvent::DelegationStarted`]; otherwise it seeds a [`ToolTrace`] and a
/// [`RunEvent::ToolStarted`]. A [`Notification::ToolCallFinished`] carries only
/// the call id, so its role is recovered from the same recorder / started map to
/// emit the matching finished (or failed) event; an external delegation that
/// completed also emits one [`RunEvent::DelegationArtifact`] per reported
/// artifact and folds those artifacts into the run output.
///
/// A `ToolCallFinished` whose call id was never seen as a `ToolCallStarted`
/// (and is not a delegation) is a call the approval gate denied before it ever
/// started: it emits **no** `ToolFinished`, so a denied tool leaves no tool
/// lifecycle event on the non-streaming path — exactly as on the streaming path,
/// where a denied call never reaches the tool handler. The paused approval is
/// still surfaced separately by [`weave_approval_events`].
pub(crate) fn collect_traces(
    notifications: &[Notification],
    recorder: &DelegationRecorder,
) -> CollectedTraces {
    let recorded = recorder
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    let mut tool_calls = Vec::new();
    let mut delegations = Vec::new();
    let mut subagent_usage = crate::model::usage::Usage::default();
    let mut external_usage = crate::model::usage::Usage::default();
    let mut artifacts = Vec::new();
    let mut events = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut external_approval_denied = false;
    let mut external_sessions: HashMap<String, RetainedExternalSession> = HashMap::new();

    for record in recorded.values() {
        if !record.is_external {
            continue;
        }
        if record.approval_denied {
            external_approval_denied = true;
        }
        let status = match record.trace.status {
            DelegationStatus::Completed => ExternalDelegateStatus::Completed,
            DelegationStatus::Failed => ExternalDelegateStatus::Failed,
        };
        external_sessions.insert(
            record.trace.delegate.clone(),
            RetainedExternalSession {
                status,
                session: record.session.clone(),
                artifacts: record.artifacts.clone(),
            },
        );
    }

    for notification in notifications {
        match notification {
            Notification::ToolCallStarted(started) => {
                let call_id = started.call_id().to_string();
                if let Some(record) = recorded.get(&call_id) {
                    delegations.push(record.trace.clone());
                    if record.is_external {
                        external_usage.merge(record.trace.usage.clone());
                    } else {
                        subagent_usage.merge(record.trace.usage.clone());
                    }
                    events.push(RunEvent::DelegationStarted(record.trace.clone()));
                } else {
                    let name = started.call().name.clone();
                    names.insert(call_id.clone(), name.clone());
                    let trace = ToolTrace { name, call_id };
                    tool_calls.push(trace.clone());
                    events.push(RunEvent::ToolStarted(trace));
                }
            }
            Notification::ToolCallFinished(finished) => {
                let call_id = finished.call_id().to_string();
                if let Some(record) = recorded.get(&call_id) {
                    match record.trace.status {
                        DelegationStatus::Completed => {
                            for artifact in &record.artifacts {
                                artifacts.push(artifact.clone());
                                events.push(RunEvent::DelegationArtifact(artifact.clone()));
                            }
                            events.push(RunEvent::DelegationFinished(record.trace.clone()));
                        }
                        DelegationStatus::Failed => {
                            events.push(RunEvent::DelegationFailed(record.trace.clone()));
                        }
                    }
                } else if let Some(name) = names.get(&call_id).cloned() {
                    events.push(RunEvent::ToolFinished(ToolTrace { name, call_id }));
                }
                // A `ToolCallFinished` with no recorded `ToolCallStarted` name
                // (and no delegation record) belongs to a call the approval gate
                // denied before it ever started: it produced no `ToolStarted`, so
                // it emits no `ToolFinished` either, keeping the non-streaming
                // path's tool lifecycle identical to the streaming path (which
                // never invokes the tool handler for a denied call). The paused
                // approval itself is still surfaced by `weave_approval_events`.
            }
            _ => {}
        }
    }

    CollectedTraces {
        tool_calls,
        delegations,
        subagent_usage,
        external_usage,
        artifacts,
        events,
        external_approval_denied,
        external_sessions,
    }
}

/// The outcome of one rules-routed delegation drive (`docs/facade-api.md` §13.2).
///
/// Shared by [`Agent::run_full`] and the streaming path: the [`RunOutput`] is the
/// terminal result to return (or yield as `Done`), while the
/// [`RecordedDelegation`] lets the caller retain an external delegate's session
/// facts (§15.2).
pub(crate) struct RulesRoutedDrive {
    /// The terminal run output assembled from the drive.
    pub output: RunOutput,
    /// The recorded delegation (trace, artifacts, session, denial flag).
    pub record: RecordedDelegation,
}

/// Drives one rules-routed delegation and assembles its terminal output.
///
/// The delegate is driven through the shared [`DelegationToolHandler`] using a
/// framework call id minted from `ids`, then its recorded trace, usage, and
/// artifacts are projected into a single-delegation [`RunOutput`]. A managed
/// external delegate the approval policy denied fails with
/// [`FacadeError::ApprovalDenied`] (§9.2).
pub(crate) async fn drive_rules_routed(
    handler: &DelegationToolHandler,
    recorder: &DelegationRecorder,
    ids: &FacadeIds,
    target: &RulesRoutedTarget,
    task: String,
    ctx: &RunContext,
) -> Result<RulesRoutedDrive, FacadeError> {
    let (record, summary) = run_one_delegation(handler, recorder, ids, target, task, ctx).await?;

    // A denied external delegate surfaces as a run-level error, matching the
    // model-routed path (§9.2).
    if record.approval_denied {
        return Err(FacadeError::ApprovalDenied);
    }

    let output = build_rules_routed_output(&record, summary);
    Ok(RulesRoutedDrive { output, record })
}

/// Drives one delegate through the shared [`DelegationToolHandler`] and returns
/// its recorded trace plus the folded summary text.
///
/// The delegate is fulfilled under a framework call id minted from `ids`; the
/// resulting [`RecordedDelegation`] is read back from `recorder` and the summary
/// is extracted from the tool result (or the classified error on failure). The
/// caller decides how to treat an approval denial or a failed status; this
/// helper never short-circuits so a dispatcher loop can inspect every run.
async fn run_one_delegation(
    handler: &DelegationToolHandler,
    recorder: &DelegationRecorder,
    ids: &FacadeIds,
    target: &RulesRoutedTarget,
    task: String,
    ctx: &RunContext,
) -> Result<(RecordedDelegation, String), FacadeError> {
    let call_id = ids.fresh_tool_call_id();
    let key = call_id.to_string();
    let result = handler
        .fulfill_rules_routed(call_id, target, task, ctx)
        .await;

    let record = recorder
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&key)
        .cloned()
        .ok_or_else(|| {
            FacadeError::InvalidState("facade-routed delegation was not recorded".to_owned())
        })?;

    let summary = rules_routed_summary(&result);
    Ok((record, summary))
}

/// Projects a single recorded rules-routed delegation into a [`RunOutput`].
///
/// The supervisor took no LLM step, so its usage is zero and the delegate's
/// usage is attributed to the subagent or external slice; the delegation trace,
/// artifacts, and bracketing events mirror a model-routed delegation exactly.
fn build_rules_routed_output(record: &RecordedDelegation, summary: String) -> RunOutput {
    let mut events = vec![RunEvent::DelegationStarted(record.trace.clone())];
    let mut usage = UsageSummary::from_supervisor(crate::model::usage::Usage::default());
    if record.is_external {
        usage.add_external(record.trace.usage.clone());
    } else {
        usage.add_subagent(record.trace.usage.clone());
    }
    match record.trace.status {
        DelegationStatus::Completed => {
            for artifact in &record.artifacts {
                events.push(RunEvent::DelegationArtifact(artifact.clone()));
            }
            events.push(RunEvent::DelegationFinished(record.trace.clone()));
        }
        DelegationStatus::Failed => {
            events.push(RunEvent::DelegationFailed(record.trace.clone()));
        }
    }
    RunOutput {
        reply: Reply::from_parts(summary, None, None),
        response: None,
        usage,
        tool_calls: Vec::new(),
        delegations: vec![record.trace.clone()],
        artifacts: record.artifacts.clone(),
        events,
    }
}

/// Extracts the delegate's summary text (or, on failure, its classified error
/// message) from a fulfilled rules-routed delegation.
fn rules_routed_summary(result: &RequirementResult) -> String {
    match result {
        RequirementResult::Tool(Ok(response)) => response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        RequirementResult::Tool(Err(error)) => error.to_string(),
        _ => String::new(),
    }
}

/// A shared capability tag for the two workers in a dispatcher roster.
///
/// The facade dispatcher is a fixed two-tier cheap→strong loop rather than a
/// capability-routed roster, so both workers advertise the same provider-neutral
/// [`Capability::Custom`] tag; the escalation decision turns purely on cost tier
/// and the primary worker's configured escalation target.
fn dispatcher_capability() -> Capability {
    Capability::Custom("dispatch".to_owned())
}

/// The outcome of one dispatcher-routed drive (`docs/facade-api.md` §13.3).
///
/// Shared by [`Agent::run_full`] and the streaming path: `output` is the
/// terminal result to return (or yield as `Done`), while `records` carries every
/// worker/verifier [`RecordedDelegation`] so the caller can retain each external
/// delegate's session facts (§15.2).
pub(crate) struct DispatcherDrive {
    /// The terminal run output assembled from the loop.
    pub output: RunOutput,
    /// Every delegation recorded during the loop, in run order.
    pub records: Vec<RecordedDelegation>,
}

/// Accumulates the ordered [`RunOutput`] pieces of a dispatcher loop.
#[derive(Default)]
struct DispatcherAccumulator {
    events: Vec<RunEvent>,
    delegations: Vec<DelegationTrace>,
    artifacts: Vec<ArtifactRef>,
    usage: UsageSummary,
    records: Vec<RecordedDelegation>,
}

impl DispatcherAccumulator {
    /// Folds one recorded delegation into the accumulator, appending its
    /// bracketing events, trace, artifacts, usage, and record exactly as a
    /// model- or rules-routed delegation would report them.
    fn record(&mut self, record: &RecordedDelegation) {
        self.events
            .push(RunEvent::DelegationStarted(record.trace.clone()));
        if record.is_external {
            self.usage.add_external(record.trace.usage.clone());
        } else {
            self.usage.add_subagent(record.trace.usage.clone());
        }
        match record.trace.status {
            DelegationStatus::Completed => {
                for artifact in &record.artifacts {
                    self.artifacts.push(artifact.clone());
                    self.events
                        .push(RunEvent::DelegationArtifact(artifact.clone()));
                }
                self.events
                    .push(RunEvent::DelegationFinished(record.trace.clone()));
            }
            DelegationStatus::Failed => {
                self.events
                    .push(RunEvent::DelegationFailed(record.trace.clone()));
            }
        }
        self.delegations.push(record.trace.clone());
        self.records.push(record.clone());
    }
}

/// Drives one task through the dispatcher cheap→verify→strong escalation loop
/// and assembles its terminal output (`docs/facade-api.md` §13.3).
///
/// The primary worker runs first; when a verifier is configured its verdict (or
/// a worker's own failure) escalates to the stronger worker, capped at
/// `config.max_attempts`. The escalation *decision* is delegated to
/// `agent::external::Escalator` (§19). A managed external delegate the approval
/// policy denies fails with [`FacadeError::ApprovalDenied`] (§9.2).
///
/// `evaluator` and `verifier` are the optional host-injected AI-routing /
/// AI-verification seams (§19). When `verifier` is present it both backs the
/// [`Escalator`] and is consulted after each worker run as an additional verdict
/// source (rejecting composes with worker failure and the verifier delegate's
/// token). When `evaluator` is present it chooses the escalation target instead
/// of the built-in roster logic. Both absent reproduces Milestone 5 exactly.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_dispatcher_routed(
    handler: &DelegationToolHandler,
    recorder: &DelegationRecorder,
    ids: &FacadeIds,
    config: &DispatcherConfig,
    targets: &HashMap<String, RulesRoutedTarget>,
    task: String,
    ctx: &RunContext,
    evaluator: Option<SharedTaskEvaluator>,
    verifier: Option<SharedVerifier>,
) -> Result<DispatcherDrive, FacadeError> {
    // An injected Verifier replaces the built-in inert ScriptedVerifier::passing()
    // seam inside the Escalator (§19); absent one the engine behaves as M5.
    let escalation_verifier: SharedVerifier = verifier
        .clone()
        .unwrap_or_else(|| Arc::new(ScriptedVerifier::passing()));
    let escalator = Escalator::new(escalation_verifier).with_budget_headroom(0);
    let roster = build_dispatcher_roster(config, ids);

    let mut acc = DispatcherAccumulator::default();
    let mut final_summary = String::new();
    let mut current = config.primary().to_owned();

    for attempt in 1..=config.max_attempts() {
        let worker = fetch_target(targets, &current)?;
        let (record, summary) =
            run_one_delegation(handler, recorder, ids, worker, task.clone(), ctx).await?;
        if record.approval_denied {
            return Err(FacadeError::ApprovalDenied);
        }
        acc.record(&record);
        final_summary = summary.clone();
        let worker_failed = record.trace.status == DelegationStatus::Failed;

        // A clean worker run that the verifier delegate (if any) and any injected
        // verifier both accept ends the loop.
        let rejected = worker_failed
            || run_verifier(
                handler, recorder, ids, config, targets, &task, &summary, ctx, &mut acc,
            )
            .await?
            || injected_verifier_rejects(verifier.as_ref(), &current, worker_failed);
        if !rejected {
            break;
        }

        // Rejected: escalate to the stronger worker while attempts remain and the
        // routing decision offers a target.
        if attempt >= config.max_attempts() {
            break;
        }
        let next = match evaluator.as_ref() {
            Some(evaluator) => {
                injected_escalation_target(evaluator.as_ref(), roster.as_ref(), targets, &current)
            }
            None => dispatcher_escalation_target(&escalator, roster.as_ref(), &current, ctx, ids)?,
        };
        let Some(next) = next else {
            break;
        };
        acc.events.push(RunEvent::Escalated(EscalationTrace {
            from: current.clone(),
            to: next.clone(),
        }));
        current = next;
    }

    let output = RunOutput {
        reply: Reply::from_parts(final_summary, None, None),
        response: None,
        usage: acc.usage,
        tool_calls: Vec::new(),
        delegations: acc.delegations,
        artifacts: acc.artifacts,
        events: acc.events,
    };
    Ok(DispatcherDrive {
        output,
        records: acc.records,
    })
}

/// Runs the configured verifier (if any) against a worker's `summary`, folding
/// its delegation into `acc`, and returns whether it requests an escalation.
///
/// A verifier rejects when its delegation fails or its reply carries the
/// [`DISPATCHER_ESCALATE_MARKER`] token (§13.3). With no verifier configured a
/// clean worker run is always accepted.
#[allow(clippy::too_many_arguments)]
async fn run_verifier(
    handler: &DelegationToolHandler,
    recorder: &DelegationRecorder,
    ids: &FacadeIds,
    config: &DispatcherConfig,
    targets: &HashMap<String, RulesRoutedTarget>,
    task: &str,
    worker_summary: &str,
    ctx: &RunContext,
    acc: &mut DispatcherAccumulator,
) -> Result<bool, FacadeError> {
    let Some(verifier_name) = config.verifier() else {
        return Ok(false);
    };
    let verifier = fetch_target(targets, verifier_name)?;
    let brief = verifier_brief(task, worker_summary);
    let (record, summary) =
        run_one_delegation(handler, recorder, ids, verifier, brief, ctx).await?;
    if record.approval_denied {
        return Err(FacadeError::ApprovalDenied);
    }
    let failed = record.trace.status == DelegationStatus::Failed;
    acc.record(&record);
    Ok(failed || verifier_requests_escalation(&summary))
}

/// Looks up a dispatcher target by name, erroring defensively if it is missing
/// (names are validated at build time, §13.3).
fn fetch_target<'a>(
    targets: &'a HashMap<String, RulesRoutedTarget>,
    name: &str,
) -> Result<&'a RulesRoutedTarget, FacadeError> {
    targets.get(name).ok_or_else(|| {
        FacadeError::InvalidState(format!("dispatcher delegate `{name}` is not registered"))
    })
}

/// Builds the verifier's task brief: the original task plus the worker's output
/// and the escalation-token protocol the facade interprets (§13.3).
fn verifier_brief(task: &str, worker_summary: &str) -> String {
    format!(
        "Review the following worker output for the task and decide whether it is acceptable.\n\n\
         Task:\n{task}\n\nWorker output:\n{worker_summary}\n\n\
         If the work is insufficient and must be redone by a stronger worker, reply with the word \
         ESCALATE; otherwise approve it."
    )
}

/// Reports whether a verifier's reply requests an escalation, i.e. contains the
/// case-insensitive [`DISPATCHER_ESCALATE_MARKER`] token (§13.3).
fn verifier_requests_escalation(summary: &str) -> bool {
    summary.to_lowercase().contains(DISPATCHER_ESCALATE_MARKER)
}

/// Builds the two-worker escalation roster for a dispatcher config, or `None`
/// when no escalation target is configured (nothing to escalate to).
///
/// The primary is registered [`CostTier::Cheap`] with an escalation rule pointing
/// at the stronger worker; the stronger worker is [`CostTier::Premium`] and
/// terminal. This is exactly the shape `agent::external::Escalator::assess`
/// resolves an upward escalation from.
fn build_dispatcher_roster(config: &DispatcherConfig, ids: &FacadeIds) -> Option<WorkerRoster> {
    let strong = config.escalate_to()?;
    let mut roster = WorkerRoster::new();
    let capability = dispatcher_capability();
    let spec = AgentSpecRef(ids.agent_id());

    roster.register(
        WorkerProfile::new(
            strong,
            [capability.clone()],
            CostTier::Premium,
            EscalationRules::none(),
        ),
        spec,
    );
    roster.register(
        WorkerProfile::new(
            config.primary(),
            [capability],
            CostTier::Cheap,
            EscalationRules::new(
                [
                    EscalationTrigger::ReviewRejected,
                    EscalationTrigger::TestFailure,
                    EscalationTrigger::Timeout,
                    EscalationTrigger::LowConfidence,
                ],
                Some(WorkerProfileRef::new(strong)),
                false,
            ),
        ),
        spec,
    );
    Some(roster)
}

/// Builds the provider-neutral [`TaskDescriptor`] the facade uses when it asks
/// the escalation engine or an injected hook to weigh a dispatcher task.
fn dispatcher_task_descriptor() -> TaskDescriptor {
    TaskDescriptor::new(
        dispatcher_capability(),
        ImpactScope::SingleFile,
        PermissionRisk::Low,
        Uncertainty::Clear,
    )
}

/// Asks an injected [`Verifier`] whether the worker `current` just produced an
/// output that should be rejected (§19), returning `false` when no verifier is
/// injected so the Milestone 5 verdict is preserved.
///
/// The verifier is consulted directly (not gated on
/// [`TaskDescriptor::warrants_verification`]) because the host injected it
/// deliberately. A `worker_failed` run is reported as a failing
/// [`WorkerReport`] so a verifier can key off the failure; otherwise a clean
/// report is passed and the verdict is entirely the verifier's.
fn injected_verifier_rejects(
    verifier: Option<&SharedVerifier>,
    current: &str,
    worker_failed: bool,
) -> bool {
    let Some(verifier) = verifier else {
        return false;
    };
    let descriptor = dispatcher_task_descriptor();
    let worker = WorkerProfileRef::new(current);
    let report = if worker_failed {
        WorkerReport::failed(worker, EscalationTrigger::ReviewRejected)
    } else {
        WorkerReport::succeeded(worker)
    };
    verifier.verify(&descriptor, &report).is_some()
}

/// Asks an injected [`TaskEvaluator`] which worker a rejected task escalates to
/// (§19), returning the target delegate name or `None` to decline.
///
/// The evaluator picks from the dispatcher roster (primary plus the configured
/// escalation target). A `None` roster (no escalation configured), an evaluator
/// that declines, or one that names the `current` worker or a delegate that is
/// not registered all mean "do not escalate".
fn injected_escalation_target(
    evaluator: &(dyn TaskEvaluator + Send + Sync),
    roster: Option<&WorkerRoster>,
    targets: &HashMap<String, RulesRoutedTarget>,
    current: &str,
) -> Option<String> {
    let roster = roster?;
    let descriptor = dispatcher_task_descriptor();
    let choice = evaluator.evaluate(&descriptor, roster)?;
    let name = choice.id();
    if name == current || !targets.contains_key(name) {
        return None;
    }
    Some(name.to_owned())
}

/// Asks `agent::external::Escalator` which stronger worker to escalate to after
/// `current` was rejected, returning the target delegate name or `None`.
///
/// A `None` roster (no escalation configured), an escalation the engine declines
/// (`Accept` / `Human` / `Exhausted`), or a `current` worker the roster does not
/// know all mean "do not escalate".
fn dispatcher_escalation_target<V: Verifier>(
    escalator: &Escalator<V>,
    roster: Option<&WorkerRoster>,
    current: &str,
    ctx: &RunContext,
    ids: &FacadeIds,
) -> Result<Option<String>, FacadeError> {
    let Some(roster) = roster else {
        return Ok(None);
    };
    let report = WorkerReport::failed(
        WorkerProfileRef::new(current),
        EscalationTrigger::ReviewRejected,
    );
    let descriptor = dispatcher_task_descriptor();
    let gate = HumanGate::new(ids.step_id(), ids.agent_id());
    match escalator.assess(&descriptor, &report, roster, ctx, &gate) {
        Ok(EscalationOutcome::Reassign(choice)) => Ok(Some(choice.worker().id().to_owned())),
        Ok(_) => Ok(None),
        // The `current` worker is not in the roster (e.g. already the strong
        // worker, whose profile is terminal): nothing further to escalate to.
        Err(EscalationError::UnknownWorker { .. }) => Ok(None),
        Err(error) => Err(FacadeError::InvalidState(error.to_string())),
    }
}

/// Concatenates the text of every [`ContentBlock::Text`] block in a user
/// message, so a rules-routed delegation can match keywords against it (§13.2).
pub(crate) fn user_message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extracts the final assistant text, aggregated usage, and last stop reason of
/// the most recently committed turn.
pub(crate) fn final_turn_summary(
    conversation: &Conversation,
) -> (
    String,
    crate::model::usage::Usage,
    Option<crate::model::normalized::StopReason>,
) {
    let Some(turn) = conversation.turns().last() else {
        return (String::new(), crate::model::usage::Usage::default(), None);
    };
    let text = turn
        .messages()
        .last()
        .map(|message| {
            message
                .payload()
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let usage = turn.meta().usage().clone();
    let stop_reason = turn
        .meta()
        .responses()
        .last()
        .map(|response| *response.stop_reason().value());
    (text, usage, stop_reason)
}
