//! Sans-io tool phase for [`DefaultAgentMachine`](super::DefaultAgentMachine).
//!
//! When an LLM step returns tool-use blocks, [`fold_llm_response`] hands control
//! to [`begin_tool_phase`], which mirrors the legacy loop's tool orchestration
//! without ever awaiting: it *requests* each tool execution and each human
//! approval as a [`Requirement`] and parks on the matching [`LoopCursor`]. A
//! driver fulfils the requirement and feeds the result back through
//! [`StepInput::Resume`](crate::agent::StepInput::Resume), at which point the
//! phase folds the [`ToolResponse`] into the pending turn and advances.
//!
//! # Phase shape
//!
//! [`begin_tool_phase`] freezes the assistant's tool calls, registers them with
//! the Conversation, and splits them by [`ApprovalRequirement`] into a batch of
//! auto-approved calls and a queue of calls that must first be approved.
//! [`advance_tool_phase`] then drives the phase to its next blocking point:
//!
//! 1. If any auto-approved calls remain, drain **all** of them as one
//!    [`RequirementKind::NeedTool`] batch and park on
//!    [`LoopCursor::AwaitingTool`].
//! 2. Otherwise, pop the next call needing approval and emit a single
//!    [`RequirementKind::NeedInteraction`], parking on
//!    [`LoopCursor::AwaitingApproval`].
//! 3. Otherwise the phase is drained: emit the tool step boundary and start the
//!    next LLM step (or fail on the per-turn step limit).
//!
//! Firing every auto-approved call in one batch is what keeps the phase inside
//! the legal cursor transitions: because the auto batch is emitted exactly once,
//! a later advance only ever re-enters `AwaitingTool` from `AwaitingApproval`
//! (an approval that was granted), never `AwaitingTool → AwaitingTool`.
//!
//! # Caveats
//!
//! - **Batch ordering.** Auto-approved calls run before calls needing approval,
//!   rather than in strict model call order. Results correlate by tool-call id,
//!   so the assembled turn is identical; only the emission order differs. This
//!   matches the effect-model's batch semantics (see
//!   `docs/agent-effect-migration.md`).
//! - **Ephemeral scratch.** The [`ToolPhase`] lives in the non-serialized
//!   [`InFlight`] scratch, exactly like M2-3's in-flight assistant id. A machine
//!   serialized mid-batch loses which calls have already resolved; durable
//!   mid-turn resumption is a driver/persistence concern deferred to M3+.

use super::DefaultAgentMachine;
use super::error::StepError;
use crate::{
    agent::{
        ApprovalDecision, ApprovalRequirement, ApprovalResponse, CancelRecoveryReason,
        CursorRequirement, Interaction, LoopCursor, LoopDoneReason, Notification, Requirement,
        RequirementId, RequirementKind, RequirementKindTag, RequirementResolution,
        RequirementResult, StepBoundary, StepId, StepOutcome, StepRejectReason, ToolCallFinished,
        ToolCallStarted, ToolFailurePolicy, ToolWaitRequirements,
        approval::approval_response_for_decision,
    },
    conversation::{
        CancelDisposition, CancelledToolResult, ConversationMessage, MessageId, PendingTurn,
        ToolCallId, ToolCallMapping,
    },
    model::{content::ContentBlock, message::Role, tool::ToolCall},
};
use std::collections::{BTreeMap, VecDeque};

/// Scratch state for the turn currently in flight.
///
/// Mirrors the legacy segment's stack locals: it carries the current LLM step's
/// assistant message id, the count of LLM steps started this turn (for the
/// per-turn step limit), and the active [`ToolPhase`], if any. It lives only
/// while a turn is unfinished and is therefore not part of the serializable
/// [`AgentState`](crate::agent::AgentState).
#[derive(Debug)]
pub(super) struct InFlight {
    assistant_message_id: MessageId,
    steps_started: u32,
    tools: Option<ToolPhase>,
}

impl InFlight {
    /// Opens scratch state for a fresh turn whose first LLM step has started.
    pub(super) const fn new(assistant_message_id: MessageId) -> Self {
        Self {
            assistant_message_id,
            steps_started: 1,
            tools: None,
        }
    }

    /// Reconstructs in-flight scratch from persisted turn facts, anchoring on a
    /// frozen assistant message rather than the (non-serialized) live scratch.
    ///
    /// This is the [`InFlight`] half of
    /// [`rebuild_scratch_from_state`](super::DefaultAgentMachine::rebuild_scratch_from_state)
    /// (effect-refine doc §3.4). Under 落点 2 the scratch is intentionally not
    /// serialized, so a machine parked mid-turn re-derives it from the durable
    /// [`Conversation`](crate::conversation::Conversation) pending transaction.
    /// Two pieces are recoverable from committed facts and rebuilt here:
    ///
    /// - `assistant_message_id`: the id of the most recently *frozen* assistant
    ///   message. For an [`AwaitingTool`](crate::agent::LoopCursor::AwaitingTool)
    ///   / [`AwaitingApproval`](crate::agent::LoopCursor::AwaitingApproval) park
    ///   this is exactly the tool-use step that is in flight; for a *continuation*
    ///   [`StreamingStep`](crate::agent::LoopCursor::StreamingStep) it is the
    ///   previous step's assistant (an anchor, not the outstanding step's yet
    ///   un-minted id — see the limitation below).
    /// - `steps_started`: the count of frozen assistant messages plus one when a
    ///   fresh assistant is still outstanding (`awaiting_unfrozen_assistant`),
    ///   i.e. a `StreamingStep` whose LLM response has not been folded.
    ///
    /// # Limitations under 落点 2
    ///
    /// - The active [`ToolPhase`] (`running` / `awaiting_approval` detail) is
    ///   *not* reconstructed (`tools: None`): mid-batch precise recovery is a
    ///   driver/persistence concern deferred to M3+ (see the module docs and
    ///   effect-refine doc §3.5). A rebuilt in-tool park is therefore a faithful
    ///   *phase marker*, not a resumable tool batch.
    /// - The pre-allocated `MessageId` a `StreamingStep` will assign to its
    ///   outstanding (still un-frozen) assistant is host-supplied and not
    ///   persisted, so it cannot be recovered. A first LLM step (only the user
    ///   message frozen, no assistant to anchor on) therefore returns [`None`];
    ///   the driver re-establishes that identity on restore.
    pub(super) fn rebuild_from_pending(
        pending: &PendingTurn,
        awaiting_unfrozen_assistant: bool,
    ) -> Option<Self> {
        let frozen_assistant_ids: Vec<MessageId> = pending
            .messages()
            .iter()
            .filter(|message| message.payload().role == Role::Assistant)
            .map(ConversationMessage::id)
            .collect();
        let anchor = *frozen_assistant_ids.last()?;
        let steps_started =
            frozen_assistant_ids.len() as u32 + u32::from(awaiting_unfrozen_assistant);
        Some(Self::restored(anchor, steps_started))
    }

    /// Builds restored in-flight scratch with no active tool phase.
    fn restored(assistant_message_id: MessageId, steps_started: u32) -> Self {
        Self {
            assistant_message_id,
            steps_started,
            tools: None,
        }
    }

    /// Returns the assistant message id of the current LLM step.
    pub(super) const fn assistant_message_id(&self) -> MessageId {
        self.assistant_message_id
    }
}

/// The tool batch produced by a single LLM step, tracked to quiescence.
#[derive(Debug)]
struct ToolPhase {
    /// The LLM step that produced these tool calls; every notification and the
    /// closing step boundary reference it.
    step_id: StepId,
    /// Auto-approved calls not yet emitted; drained as one `NeedTool` batch.
    auto_pending: Vec<ToolSlot>,
    /// Calls awaiting approval, emitted one `NeedInteraction` at a time.
    approval_pending: VecDeque<ToolSlot>,
    /// Emitted `NeedTool` requirements not yet resolved, keyed by requirement id
    /// so an out-of-order batch resume routes to the right call.
    running: BTreeMap<RequirementId, ToolSlot>,
    /// The single approval currently outstanding, if any.
    awaiting_approval: Option<(RequirementId, ToolSlot)>,
}

/// One tool call threaded through the phase from freeze to result.
#[derive(Clone, Debug)]
struct ToolSlot {
    /// Provider-assigned call id, used to stamp synthesized denial responses.
    provider_call_id: String,
    /// Framework tool-call identity paired through the Conversation.
    call_id: ToolCallId,
    /// Pre-allocated id of the tool-result message this call will append.
    result_message_id: MessageId,
    /// The provider-neutral tool call selected by the model.
    call: ToolCall,
    /// Approval requirement classified for this call.
    approval: ApprovalRequirement,
}

impl DefaultAgentMachine {
    /// Opens a tool phase from the just-frozen assistant tool-use response.
    ///
    /// Registers the model's tool calls with the Conversation, classifies each
    /// by approval policy, then advances to the first blocking point. Entered
    /// with the cursor on [`LoopCursor::StreamingStep`] (the LLM step that
    /// produced the calls).
    pub(super) fn begin_tool_phase(&mut self, step_id: StepId) -> Result<StepOutcome, StepError> {
        let calls = self.pending_tool_calls().map_err(StepError::Protocol)?;

        // Map provider ids → framework ids and register the open calls.
        let mut mappings = Vec::with_capacity(calls.len());
        let mut call_ids = Vec::with_capacity(calls.len());
        for call in &calls {
            let call_id = self
                .tool_ids
                .tool_call_id(call)
                .map_err(|error| StepError::Protocol(format!("tool id unavailable: {error}")))?;
            mappings.push(ToolCallMapping::new(call.id.clone(), call_id));
            call_ids.push(call_id);
        }
        self.state
            .conversation_mut()
            .register_tool_calls(mappings)?;

        // Build one slot per call, split by approval requirement.
        let mut auto_pending = Vec::new();
        let mut approval_pending = VecDeque::new();
        for (call, call_id) in calls.into_iter().zip(call_ids) {
            let result_message_id = self
                .tool_ids
                .tool_result_message_id(call_id, &call)
                .map_err(|error| StepError::Protocol(format!("tool id unavailable: {error}")))?;
            let approval = self.approval_policy.approval_requirement(call_id, &call);
            let slot = ToolSlot {
                provider_call_id: call.id.clone(),
                call_id,
                result_message_id,
                call,
                approval: approval.clone(),
            };
            match approval {
                ApprovalRequirement::AutoApprove => auto_pending.push(slot),
                ApprovalRequirement::RequireApproval { .. } => approval_pending.push_back(slot),
            }
        }

        let phase = ToolPhase {
            step_id,
            auto_pending,
            approval_pending,
            running: BTreeMap::new(),
            awaiting_approval: None,
        };
        match self.in_flight_mut() {
            Some(in_flight) => in_flight.tools = Some(phase),
            None => {
                return Err(StepError::Protocol(
                    "tool phase opened without an in-flight turn".to_owned(),
                ));
            }
        }

        self.advance_tool_phase(Vec::new())
    }

    /// Drives the active tool phase to its next blocking point.
    ///
    /// `notifications` carries any events produced earlier in the same step (for
    /// example the [`ToolCallFinished`] that unblocked this advance) so they ride
    /// out with the next requirement or step boundary.
    fn advance_tool_phase(
        &mut self,
        notifications: Vec<Notification>,
    ) -> Result<StepOutcome, StepError> {
        let Some(step_id) = self.tool_phase().map(|phase| phase.step_id) else {
            return Err(StepError::Protocol(
                "tool phase advanced without an active phase".to_owned(),
            ));
        };

        // 1. Emit every auto-approved call as one batch (fires at most once).
        let auto = self
            .tool_phase_mut()
            .map(|phase| std::mem::take(&mut phase.auto_pending))
            .unwrap_or_default();
        if !auto.is_empty() {
            return self.emit_tool_batch(step_id, auto, notifications);
        }

        // 2. Emit the next call needing approval.
        let next_approval = self
            .tool_phase_mut()
            .and_then(|phase| phase.approval_pending.pop_front());
        if let Some(slot) = next_approval {
            return self.emit_approval(step_id, slot, notifications);
        }

        // 3. Nothing left: close the tool step and continue with the model.
        self.finish_tool_phase(step_id, notifications)
    }

    /// Emits `slots` as one `NeedTool` batch and parks on `AwaitingTool`.
    fn emit_tool_batch(
        &mut self,
        step_id: StepId,
        slots: Vec<ToolSlot>,
        mut notifications: Vec<Notification>,
    ) -> Result<StepOutcome, StepError> {
        let mut requirements = Vec::with_capacity(slots.len());
        let mut ids: BTreeMap<ToolCallId, RequirementId> = BTreeMap::new();
        for slot in slots {
            let requirement_id = match self
                .requirement_ids
                .next_requirement_id(RequirementKindTag::Tool)
            {
                Ok(id) => id,
                Err(error) => {
                    return Ok(self.fail_with_notifications(
                        notifications,
                        format!("requirement id unavailable: {error}"),
                    ));
                }
            };

            notifications.push(Notification::ToolCallStarted(ToolCallStarted::new(
                step_id,
                slot.call_id,
                slot.call.clone(),
                None,
            )));
            requirements.push(Requirement::at_root(
                requirement_id,
                RequirementKind::NeedTool {
                    call_id: slot.call_id,
                    call: slot.call.clone(),
                },
            ));
            ids.insert(slot.call_id, requirement_id);
            if let Some(phase) = self.tool_phase_mut() {
                phase.running.insert(requirement_id, slot);
            }
        }

        let call_ids: Vec<ToolCallId> = ids.keys().copied().collect();
        let cursor = match LoopCursor::awaiting_tool(
            step_id,
            call_ids,
            Some(ToolWaitRequirements::root(ids)),
        ) {
            Ok(cursor) => cursor,
            Err(error) => {
                return Ok(self.fail_with_notifications(
                    notifications,
                    format!("cursor build failed: {error}"),
                ));
            }
        };
        if let Err(error) = self.state.transition_cursor(cursor) {
            return Ok(self.fail_with_notifications(
                notifications,
                format!("cursor transition failed: {error}"),
            ));
        }

        Ok(StepOutcome::new(notifications, requirements, true))
    }

    /// Emits `slot` as one `NeedInteraction` and parks on `AwaitingApproval`.
    fn emit_approval(
        &mut self,
        step_id: StepId,
        slot: ToolSlot,
        notifications: Vec<Notification>,
    ) -> Result<StepOutcome, StepError> {
        let requirement_id = match self
            .requirement_ids
            .next_requirement_id(RequirementKindTag::Interaction)
        {
            Ok(id) => id,
            Err(error) => {
                return Ok(self.fail_with_notifications(
                    notifications,
                    format!("requirement id unavailable: {error}"),
                ));
            }
        };

        let interaction = Interaction::approval(step_id, slot.call_id, slot.approval.clone());
        let cursor = LoopCursor::awaiting_approval(
            step_id,
            slot.call_id,
            Some(CursorRequirement::root(requirement_id)),
        );
        if let Err(error) = self.state.transition_cursor(cursor) {
            return Ok(self.fail_with_notifications(
                notifications,
                format!("cursor transition failed: {error}"),
            ));
        }

        let requirement = Requirement::at_root(
            requirement_id,
            RequirementKind::NeedInteraction {
                request: interaction,
            },
        );
        if let Some(phase) = self.tool_phase_mut() {
            phase.awaiting_approval = Some((requirement_id, slot));
        }
        Ok(StepOutcome::new(notifications, vec![requirement], true))
    }

    /// Folds a `NeedTool` result into the pending turn.
    ///
    /// Routes by requirement id so a parallel batch may resolve out of order,
    /// appends the (possibly failure-synthesized) tool response, and advances the
    /// phase once the whole batch is idle.
    pub(super) fn resume_tool(
        &mut self,
        resolution: RequirementResolution,
    ) -> Result<StepOutcome, StepError> {
        let slot = match self.tool_phase_mut() {
            Some(phase) => match phase.running.remove(&resolution.id) {
                Some(slot) => slot,
                None => {
                    return Err(StepError::Rejected(StepRejectReason::UnknownRequirement(
                        format!(
                            "resume targets requirement {}, which is not an in-flight tool call",
                            resolution.id
                        ),
                    )));
                }
            },
            None => {
                return Err(StepError::Protocol(
                    "tool result resumed without an active tool phase".to_owned(),
                ));
            }
        };
        let step_id = match self.tool_phase() {
            Some(phase) => phase.step_id,
            None => {
                return Err(StepError::Protocol(
                    "tool result resumed without an active tool phase".to_owned(),
                ));
            }
        };

        let response = match resolution.result {
            RequirementResult::Tool(Ok(response)) => response,
            RequirementResult::Tool(Err(error)) => match self.tool_failure_policy() {
                ToolFailurePolicy::ReturnErrorToModel => {
                    error.to_tool_response(slot.provider_call_id.clone())
                }
                ToolFailurePolicy::StopRun => {
                    return Err(StepError::Protocol(format!(
                        "tool `{}` failed: {error}",
                        slot.call.name
                    )));
                }
            },
            other => {
                return Err(StepError::Protocol(format!(
                    "NeedTool requirement cannot accept a `{}` result",
                    other.tag()
                )));
            }
        };

        self.state
            .conversation_mut()
            .append_tool_response(slot.result_message_id, response.clone())?;

        let finished = Notification::ToolCallFinished(ToolCallFinished::new(
            step_id,
            slot.call_id,
            response,
            None,
        ));

        if self.tool_batch_idle() {
            self.advance_tool_phase(vec![finished])
        } else {
            Ok(StepOutcome::new(vec![finished], Vec::new(), true))
        }
    }

    /// Folds a `NeedInteraction` (approval) result into the tool phase.
    ///
    /// An approval emits a single `NeedTool` for the now-allowed call; a denial,
    /// timeout, or cancellation appends a synthesized tool response and advances
    /// past the call. The transient `AwaitingApproval → AwaitingTool` bounce on
    /// the denial path keeps the cursor inside its legal transitions before the
    /// advance re-parks or finishes.
    pub(super) fn resume_approval(
        &mut self,
        expected_id: Option<RequirementId>,
        resolution: RequirementResolution,
    ) -> Result<StepOutcome, StepError> {
        // Validate the resumed id *before* taking the pending approval out of
        // the scratch: a stale or foreign id is a soft rejection, and a soft
        // rejection must leave the machine bit-for-bit untouched (M4-4).
        let requirement_id = match self
            .tool_phase()
            .and_then(|phase| phase.awaiting_approval.as_ref())
        {
            Some((requirement_id, _)) => *requirement_id,
            None => {
                return Err(StepError::Protocol(
                    "approval resumed without a pending interaction".to_owned(),
                ));
            }
        };

        if let Some(expected) = expected_id
            && resolution.id != expected
        {
            return Err(StepError::Rejected(StepRejectReason::UnknownRequirement(
                format!(
                    "resume targets requirement {}, but the machine awaits {expected}",
                    resolution.id
                ),
            )));
        }
        if resolution.id != requirement_id {
            return Err(StepError::Rejected(StepRejectReason::UnknownRequirement(
                format!(
                    "resume targets requirement {}, but the pending approval is {requirement_id}",
                    resolution.id
                ),
            )));
        }

        let slot = match self
            .tool_phase_mut()
            .and_then(|phase| phase.awaiting_approval.take())
        {
            Some((_, slot)) => slot,
            None => {
                return Err(StepError::Protocol(
                    "approval resumed without a pending interaction".to_owned(),
                ));
            }
        };
        let step_id = match self.tool_phase() {
            Some(phase) => phase.step_id,
            None => {
                return Err(StepError::Protocol(
                    "approval resumed without an active tool phase".to_owned(),
                ));
            }
        };

        let response = match resolution.result {
            RequirementResult::Interaction(response) => response,
            other => {
                return Err(StepError::Protocol(format!(
                    "NeedInteraction requirement cannot accept a `{}` result",
                    other.tag()
                )));
            }
        };

        let interaction = Interaction::approval(step_id, slot.call_id, slot.approval.clone());
        interaction.accepts_response(&response).map_err(|error| {
            StepError::Protocol(format!("interaction result rejected: {error}"))
        })?;
        let approval = ApprovalResponse::try_from(response).map_err(|error| {
            StepError::Protocol(format!("interaction result rejected: {error}"))
        })?;

        match approval.decision() {
            ApprovalDecision::Approve => self.emit_tool_batch(step_id, vec![slot], Vec::new()),
            decision @ (ApprovalDecision::Deny
            | ApprovalDecision::Timeout
            | ApprovalDecision::Cancel) => {
                let synthetic =
                    approval_response_for_decision(&slot.call, decision, approval.message());
                self.state
                    .conversation_mut()
                    .append_tool_response(slot.result_message_id, synthetic.clone())?;
                let finished = Notification::ToolCallFinished(ToolCallFinished::new(
                    step_id,
                    slot.call_id,
                    synthetic,
                    None,
                ));

                // Restore the awaiting-tool cursor for the denied call so the
                // phase leaves `AwaitingApproval` through a legal transition
                // before advancing.
                let bounce = match LoopCursor::awaiting_tool(step_id, vec![slot.call_id], None) {
                    Ok(cursor) => cursor,
                    Err(error) => {
                        return Ok(self.fail_with_notifications(
                            vec![finished],
                            format!("cursor build failed: {error}"),
                        ));
                    }
                };
                if let Err(error) = self.state.transition_cursor(bounce) {
                    return Ok(self.fail_with_notifications(
                        vec![finished],
                        format!("cursor transition failed: {error}"),
                    ));
                }

                self.advance_tool_phase(vec![finished])
            }
        }
    }

    /// Closes a drained tool phase and starts the next LLM step.
    ///
    /// Emits the tool step's boundary (mirroring the legacy loop's pending
    /// step-boundary pivot point), enforces the per-turn step limit, then
    /// allocates the next assistant/step ids and blocks on the next `NeedLlm`.
    fn finish_tool_phase(
        &mut self,
        step_id: StepId,
        mut notifications: Vec<Notification>,
    ) -> Result<StepOutcome, StepError> {
        let boundary = self.state.conversation().head();
        notifications.push(Notification::StepBoundary(StepBoundary::new(
            step_id, boundary, None,
        )));

        if let Some(in_flight) = self.in_flight_mut() {
            in_flight.tools = None;
        }

        let max_steps = self.state.current_loop_policy().max_steps().get();
        let steps_started = self
            .in_flight()
            .map_or(0, |in_flight| in_flight.steps_started);
        if steps_started >= max_steps {
            return self.finish_step_limit(notifications);
        }

        let next_step_id = match self.tool_ids.next_step_id() {
            Ok(id) => id,
            Err(error) => {
                return Ok(self.fail_with_notifications(
                    notifications,
                    format!("tool id unavailable: {error}"),
                ));
            }
        };
        let next_assistant_id = match self.tool_ids.next_assistant_message_id() {
            Ok(id) => id,
            Err(error) => {
                return Ok(self.fail_with_notifications(
                    notifications,
                    format!("tool id unavailable: {error}"),
                ));
            }
        };
        if let Some(in_flight) = self.in_flight_mut() {
            in_flight.assistant_message_id = next_assistant_id;
            in_flight.steps_started += 1;
        }

        self.block_on_llm(next_step_id, notifications)
    }

    /// Closes the turn at the per-turn step limit as a *normal* terminal
    /// (M4-4): the cursor settles on [`LoopCursor::Done`] with
    /// [`LoopDoneReason::StepLimitReached`] instead of the error cursor, and the
    /// frozen tool results are not discarded.
    ///
    /// The tool phase has just drained, so no call is open and an empty closure
    /// is the exact one [`cancel_pending`](crate::conversation::Conversation::cancel_pending)
    /// requires: [`CancelDisposition::ResumeTurn`] keeps every frozen message —
    /// the completed tool results included — in a coherent, resumable pending
    /// turn (the same preservation the tool-abandon path uses), rather than
    /// dropping the whole turn the way the former error path did.
    fn finish_step_limit(
        &mut self,
        notifications: Vec<Notification>,
    ) -> Result<StepOutcome, StepError> {
        if self.state.conversation().pending().is_some() {
            self.state
                .conversation_mut()
                .cancel_pending(CancelDisposition::ResumeTurn {
                    cancelled_results: Vec::new(),
                })?;
        }
        self.scratch = super::TurnScratch::None;
        self.state
            .transition_cursor(LoopCursor::done(LoopDoneReason::StepLimitReached))
            .map_err(StepError::CursorTransition)?;
        Ok(StepOutcome::new(notifications, Vec::new(), true))
    }

    /// Extracts the tool-use calls from the pending turn's last assistant
    /// message, mirroring the legacy loop's `extract_last_tool_calls`.
    fn pending_tool_calls(&self) -> Result<Vec<ToolCall>, String> {
        let pending = self
            .state
            .conversation()
            .pending()
            .ok_or_else(|| "tool-use finish left no pending turn".to_owned())?;
        let message = pending
            .messages()
            .last()
            .ok_or_else(|| "tool-use finish left no assistant message".to_owned())?;

        let calls = message
            .payload()
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => Some(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }),
                ContentBlock::Text { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::ToolResult { .. }
                | ContentBlock::Thinking { .. } => None,
            })
            .collect::<Vec<_>>();

        if calls.is_empty() {
            return Err(
                "assistant finish required tool mappings but no tool-use blocks were found"
                    .to_owned(),
            );
        }
        Ok(calls)
    }

    /// Returns the active tool phase, if any.
    fn tool_phase(&self) -> Option<&ToolPhase> {
        self.in_flight()
            .and_then(|in_flight| in_flight.tools.as_ref())
    }

    /// Returns a mutable view of the active tool phase, if any.
    fn tool_phase_mut(&mut self) -> Option<&mut ToolPhase> {
        self.in_flight_mut()
            .and_then(|in_flight| in_flight.tools.as_mut())
    }

    /// Reports whether the active batch has no outstanding tool or approval.
    fn tool_batch_idle(&self) -> bool {
        self.tool_phase()
            .is_some_and(|phase| phase.running.is_empty() && phase.awaiting_approval.is_none())
    }

    /// Never-resume close for an outstanding tool batch or approval.
    ///
    /// Unlike an LLM-only step, the pending turn already carries the assistant's
    /// frozen tool_use blocks, so it cannot simply be discarded — that would
    /// leave a dangling tool_use with no matching result. Instead the machine
    /// synthesizes a `Cancelled` tool result for **every** still-open call and
    /// closes the transaction with [`CancelDisposition::ResumeTurn`]. Any call
    /// that already resolved keeps its real result, which is what lets a partial
    /// tool batch be abandoned coherently. The turn then settles to
    /// [`LoopCursor::Idle`] via [`finish_cancel`](Self::finish_cancel).
    pub(super) fn abandon_tool_phase(
        &mut self,
        step_id: Option<StepId>,
    ) -> Result<StepOutcome, StepError> {
        let Some(cancelled_results) = self.open_cancelled_results() else {
            return Err(StepError::Protocol(
                "abandon reached a tool cursor without an active tool phase".to_owned(),
            ));
        };

        self.state
            .conversation_mut()
            .cancel_pending(CancelDisposition::ResumeTurn { cancelled_results })?;

        self.finish_cancel(step_id, CancelRecoveryReason::ToolInterrupted)
    }

    /// Collects a [`CancelledToolResult`] for every still-open call in the active
    /// tool phase — the queued auto batch, the emitted-but-unresolved `running`
    /// batch, the approval queue, and the single outstanding approval. These are
    /// exactly the frozen tool calls the pending turn still lacks a result for,
    /// so they match the closure [`cancel_pending`] requires for
    /// [`CancelDisposition::ResumeTurn`].
    fn open_cancelled_results(&self) -> Option<Vec<CancelledToolResult>> {
        let phase = self.tool_phase()?;
        let open = phase
            .auto_pending
            .iter()
            .chain(phase.running.values())
            .chain(phase.approval_pending.iter())
            .chain(phase.awaiting_approval.iter().map(|(_, slot)| slot))
            .map(|slot| {
                CancelledToolResult::new(
                    slot.provider_call_id.clone(),
                    slot.call_id,
                    slot.result_message_id,
                )
            })
            .collect();
        Some(open)
    }

    /// Returns the failure policy governing tool errors this turn.
    fn tool_failure_policy(&self) -> ToolFailurePolicy {
        self.state.current_loop_policy().tool_failure_policy()
    }
}
