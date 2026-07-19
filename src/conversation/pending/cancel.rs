//! Atomic cancellation of the unique pending turn.
//!
//! Cancellation never inspects or finishes an active accumulator. It prepares
//! complete synthetic tool results from frozen bookkeeping, then either drops
//! the whole transaction, resumes it at an assistant boundary, or submits a
//! complete candidate through the sole closed-turn validator.

mod prepare;

use self::prepare::{
    cancelled_turn_data, prepare_cancellation, retained_id_sets, validate_final_message_id,
};
use super::{FrozenMessage, PendingMessage};
use crate::{
    client::Response,
    conversation::{
        CancelError, Conversation, ConversationError, MessageId, ToolCallId, TurnId, TurnMeta,
    },
    model::content::ContentBlock,
};

/// Stable text stored inside every synthetic cancelled tool result.
pub const CANCELLED_TOOL_RESULT_TEXT: &str = "Tool execution was cancelled before completion.";

/// Caller-supplied identities for closing one frozen open tool call.
///
/// Calls awaiting their first framework mapping use `call_id` to establish it.
/// Already-mapped calls require the exact same id, preventing cancellation from
/// silently changing an existing provider/framework correlation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelledToolResult {
    provider_call_id: String,
    call_id: ToolCallId,
    message_id: MessageId,
}

impl CancelledToolResult {
    /// Creates the identity bundle for one synthetic cancelled result.
    #[must_use]
    pub fn new(
        provider_call_id: impl Into<String>,
        call_id: ToolCallId,
        message_id: MessageId,
    ) -> Self {
        Self {
            provider_call_id: provider_call_id.into(),
            call_id,
            message_id,
        }
    }

    /// Returns the provider identity copied from frozen tool-use content.
    #[must_use]
    pub fn provider_call_id(&self) -> &str {
        &self.provider_call_id
    }

    /// Returns the framework identity retained in the explicit pairing.
    #[must_use]
    pub const fn call_id(&self) -> ToolCallId {
        self.call_id
    }

    /// Returns the external identity assigned to the synthetic result message.
    #[must_use]
    pub const fn message_id(&self) -> MessageId {
        self.message_id
    }
}

/// How cancellation should leave the unique pending transaction.
///
/// ```
/// use agent_lib::conversation::{
///     CancelDisposition, CancelledToolResult, MessageId, ToolCallId,
/// };
///
/// let call_id: ToolCallId =
///     "018f0d9c-7b6a-7c12-8f31-1234567890ab".parse().unwrap();
/// let result_message_id: MessageId =
///     "018f0d9c-7b6a-7c12-8f31-1234567890ac".parse().unwrap();
/// let disposition = CancelDisposition::ResumeTurn {
///     cancelled_results: vec![CancelledToolResult::new(
///         "provider-call-1",
///         call_id,
///         result_message_id,
///     )],
/// };
///
/// match disposition {
///     CancelDisposition::ResumeTurn { cancelled_results } => {
///         assert_eq!(cancelled_results[0].provider_call_id(), "provider-call-1");
///     }
///     _ => unreachable!(),
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CancelDisposition {
    /// Atomically discard the entire transaction and return to committed history.
    DiscardTurn,
    /// Drop any active partial and keep a coherent pending turn for another assistant.
    ResumeTurn {
        /// Exact closures for every frozen call that still lacks a result.
        cancelled_results: Vec<CancelledToolResult>,
    },
    /// Close open calls, append a complete final assistant, and commit atomically.
    CommitTurn {
        /// Exact closures for every frozen call that still lacks a result.
        cancelled_results: Vec<CancelledToolResult>,
        /// External identity for the supplied final assistant response.
        final_message_id: MessageId,
        /// Complete tool-free assistant response used instead of any active partial.
        final_response: Box<Response>,
        /// Caller metadata merged with all previously frozen response metadata.
        meta: Box<TurnMeta>,
    },
}

impl CancelDisposition {
    /// Creates a commit disposition while keeping the enum representation small.
    #[must_use]
    pub fn commit_turn(
        cancelled_results: Vec<CancelledToolResult>,
        final_message_id: MessageId,
        final_response: Response,
        meta: TurnMeta,
    ) -> Self {
        Self::CommitTurn {
            cancelled_results,
            final_message_id,
            final_response: Box::new(final_response),
            meta: Box::new(meta),
        }
    }
}

/// Observable result of one successful pending-turn cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The pending transaction was discarded without changing history.
    Discarded {
        /// Identity of the discarded pending turn.
        turn_id: TurnId,
    },
    /// The pending transaction is coherent and awaits another assistant.
    Resumed {
        /// Identity of the retained pending turn.
        turn_id: TurnId,
    },
    /// The cancellation candidate passed the validator and entered history.
    Committed {
        /// Identity of the newly committed turn.
        turn_id: TurnId,
    },
}

impl CancelOutcome {
    /// Returns the pending or committed turn affected by cancellation.
    #[must_use]
    pub const fn turn_id(self) -> TurnId {
        match self {
            Self::Discarded { turn_id }
            | Self::Resumed { turn_id }
            | Self::Committed { turn_id } => turn_id,
        }
    }
}

impl Conversation {
    /// Cancels the unique pending transaction according to `disposition`.
    ///
    /// `DiscardTurn` accepts every pending phase. `ResumeTurn` and `CommitTurn`
    /// reject an already-final `ReadyToCommit` transaction; callers can commit
    /// that value normally or discard it. Every other phase can be cancelled,
    /// including a terminal/partial assistant, unmapped frozen tool uses, and a
    /// partially answered parallel batch.
    ///
    /// All identity, mapping, response-freeze, and validator checks happen
    /// before pending state is changed. On error both committed history and the
    /// original pending transaction remain available unchanged.
    pub fn cancel_pending(
        &mut self,
        disposition: CancelDisposition,
    ) -> Result<CancelOutcome, ConversationError> {
        match disposition {
            CancelDisposition::DiscardTurn => self.discard_pending_turn(),
            CancelDisposition::ResumeTurn { cancelled_results } => {
                self.resume_cancelled_turn(&cancelled_results)
            }
            CancelDisposition::CommitTurn {
                cancelled_results,
                final_message_id,
                final_response,
                meta,
            } => self.commit_cancelled_turn(
                &cancelled_results,
                final_message_id,
                *final_response,
                *meta,
            ),
        }
    }

    /// Drops the whole pending transaction without synthesizing throwaway results.
    fn discard_pending_turn(&mut self) -> Result<CancelOutcome, ConversationError> {
        let pending = self.pending.take().ok_or(CancelError::NoPending)?;
        let turn_id = pending.id();
        drop(pending);
        self.refresh_pending_index();
        Ok(CancelOutcome::Discarded { turn_id })
    }

    /// Prepares every closure before replacing the active state in one step.
    fn resume_cancelled_turn(
        &mut self,
        cancelled_results: &[CancelledToolResult],
    ) -> Result<CancelOutcome, ConversationError> {
        let (committed_message_ids, committed_call_ids) = retained_id_sets(self);
        let pending = self.pending.as_ref().ok_or(CancelError::NoPending)?;
        let turn_id = pending.id();
        let prepared = prepare_cancellation(
            pending,
            cancelled_results,
            &committed_message_ids,
            &committed_call_ids,
            "resume a cancelled turn",
        )?;

        self.pending
            .as_mut()
            .ok_or(CancelError::NoPending)?
            .resume_after_cancel(prepared.cancelled_messages, prepared.tool_calls);
        self.refresh_pending_index();
        Ok(CancelOutcome::Resumed { turn_id })
    }

    /// Builds a complete candidate and clears pending only after checked commit.
    fn commit_cancelled_turn(
        &mut self,
        cancelled_results: &[CancelledToolResult],
        final_message_id: MessageId,
        final_response: Response,
        meta: TurnMeta,
    ) -> Result<CancelOutcome, ConversationError> {
        let (committed_message_ids, committed_call_ids) = retained_id_sets(self);
        let pending = self.pending.as_ref().ok_or(CancelError::NoPending)?;
        let prepared = prepare_cancellation(
            pending,
            cancelled_results,
            &committed_message_ids,
            &committed_call_ids,
            "commit a cancelled turn",
        )?;
        validate_final_message_id(pending, &prepared, &committed_message_ids, final_message_id)?;
        let final_message = freeze_final_response(final_message_id, final_response)?;
        reject_final_tool_use(&final_message)?;
        let data = cancelled_turn_data(pending, prepared, final_message, meta);

        let turn_id = self.commit_draft(data)?;
        self.pending = None;
        Ok(CancelOutcome::Committed { turn_id })
    }
}

/// Freezes only the caller's complete replacement response, never the partial.
fn freeze_final_response(
    message_id: MessageId,
    response: Response,
) -> Result<FrozenMessage, ConversationError> {
    let mut pending = PendingMessage::from_response(response);
    match pending.finish(message_id) {
        Ok(frozen) => Ok(frozen),
        Err(ConversationError::PendingMessage(source)) => {
            Err(CancelError::InvalidFinalResponse { source }.into())
        }
        Err(other) => Err(other),
    }
}

/// Rejects a replacement that would immediately reopen the cancelled turn.
fn reject_final_tool_use(final_message: &FrozenMessage) -> Result<(), CancelError> {
    if let Some(provider_call_id) =
        final_message
            .message()
            .payload()
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                ContentBlock::Text { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::ToolResult { .. }
                | ContentBlock::Thinking { .. }
                | ContentBlock::Unknown { .. } => None,
            })
    {
        return Err(CancelError::FinalAssistantContainsToolUse { provider_call_id });
    }
    Ok(())
}
