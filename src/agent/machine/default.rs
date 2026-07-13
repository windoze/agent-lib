//! Concrete sans-io Agent machine for the LLM step (text-only turn).
//!
//! [`DefaultAgentMachine`] is the effect-model counterpart of the legacy
//! [`DefaultAgentLoop`](crate::agent::DefaultAgentLoop) for the LLM path: instead
//! of awaiting the client internally, it *requests* one generation by handing
//! back a [`RequirementKind::NeedLlm`] and parks on
//! [`LoopCursor::StreamingStep`]. A driver fulfils that requirement and feeds the
//! [`Response`] back through [`StepInput::Resume`], at which point the machine
//! folds the response into the single active Conversation using the same checked
//! pending boundary the legacy loop uses (`start_assistant_response` →
//! `finish_assistant` → `commit_pending`).
//!
//! This milestone (M2-3) implements the text-only turn end to end
//! (`begin_turn → NeedLlm → fold Response → commit → quiescent`). Responses that
//! carry tool calls, interactions, pivots, and cancellation are out of scope
//! here and land in later milestones (M2-4 / M4); until then they resolve to a
//! classified error cursor rather than being silently ignored.
//!
//! The machine is pure: [`step`](AgentMachine::step) never `await`s and never
//! touches a client, tool, or process. The only non-serialized field is the
//! host-supplied [`RequirementIds`] allocator (the same "library never mints
//! ids" boundary as [`ToolExecutionIds`](crate::agent::ToolExecutionIds)); the
//! serializable machine state is the wrapped [`AgentState`], whose
//! [`LoopCursor`] records the outstanding [`RequirementId`](crate::agent::RequirementId).

use crate::{
    agent::{
        AgentInput, AgentMachine, AgentState, AgentUserInput, CursorRequirement, LlmStepMode,
        LoopCursor, LoopDoneReason, Notification, Requirement, RequirementIds, RequirementKind,
        RequirementKindTag, RequirementResolution, RequirementResult, StepBoundary, StepId,
        StepInput, StepOutcome, request::build_chat_request,
    },
    client::Response,
    conversation::{AssistantFinish, CancelDisposition, MessageId, TurnMeta},
};
use std::sync::Arc;

/// Sans-io Agent machine that drives text-only LLM turns.
///
/// See the [`machine`](crate::agent::machine) module docs for the effect-model
/// contract and scope.
#[derive(Debug)]
pub struct DefaultAgentMachine {
    state: AgentState,
    mode: LlmStepMode,
    requirement_ids: Arc<dyn RequirementIds>,
    /// Assistant message id of the in-flight step, carried from the opening
    /// external input to the [`Response`] fold. This mirrors the legacy
    /// `PreparedAssistantCall.assistant_message_id`: the cursor records *which*
    /// requirement the machine is stuck on, while the caller-supplied assistant
    /// id needed to freeze the response is held here for the duration of the
    /// single in-flight step.
    pending_assistant_message_id: Option<MessageId>,
}

impl DefaultAgentMachine {
    /// Creates a machine over `state`, using `mode` for the LLM transport and
    /// `requirement_ids` to stamp reified requirements.
    #[must_use]
    pub fn new(
        state: AgentState,
        mode: LlmStepMode,
        requirement_ids: Arc<dyn RequirementIds>,
    ) -> Self {
        Self {
            state,
            mode,
            requirement_ids,
            pending_assistant_message_id: None,
        }
    }

    /// Returns the LLM transport mode requested by this machine.
    #[must_use]
    pub const fn mode(&self) -> LlmStepMode {
        self.mode
    }

    /// Returns a read-only view of the wrapped serializable Agent state.
    #[must_use]
    pub const fn state(&self) -> &AgentState {
        &self.state
    }

    /// Consumes the machine and returns its serializable Agent state.
    #[must_use]
    pub fn into_state(self) -> AgentState {
        self.state
    }

    /// Opens a fresh user turn and blocks on one `NeedLlm` requirement.
    fn begin_user_turn(&mut self, user: AgentUserInput) -> StepOutcome {
        let requirement_id = match self
            .requirement_ids
            .next_requirement_id(RequirementKindTag::Llm)
        {
            Ok(id) => id,
            Err(error) => return self.fail(format!("requirement id unavailable: {error}")),
        };

        if let Err(error) = self.state.conversation_mut().begin_turn(
            user.turn_id(),
            user.message_id(),
            user.message().clone(),
        ) {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        let tools = self.state.current_tool_set().tools().to_vec();
        let request = build_chat_request(&self.state, tools, self.mode.request_stream_flag());

        let cursor = LoopCursor::streaming_step(
            user.step_id(),
            Some(CursorRequirement::root(requirement_id)),
        );
        if let Err(error) = self.state.transition_cursor(cursor) {
            return self.fail(format!("cursor transition failed: {error}"));
        }

        self.pending_assistant_message_id = Some(user.assistant_message_id());

        let requirement = Requirement::at_root(
            requirement_id,
            RequirementKind::NeedLlm {
                request,
                mode: self.mode,
            },
        );
        StepOutcome::new(Vec::new(), vec![requirement], true)
    }

    /// Feeds a fulfilled requirement result back into the in-flight LLM step.
    fn resume(&mut self, resolution: RequirementResolution) -> StepOutcome {
        let (step_id, expected_id) = match self.state.loop_cursor() {
            LoopCursor::StreamingStep(cursor) => (cursor.step_id(), cursor.requirement_id()),
            other => {
                return self.fail(format!(
                    "resume received while cursor is `{:?}`, no outstanding LLM requirement",
                    other.kind()
                ));
            }
        };

        if let Some(expected) = expected_id
            && resolution.id != expected
        {
            return self.fail(format!(
                "resume targets requirement {}, but the machine awaits {expected}",
                resolution.id
            ));
        }

        match resolution.result {
            RequirementResult::Llm(Ok(response)) => self.fold_llm_response(step_id, response),
            RequirementResult::Llm(Err(error)) => {
                self.fail(format!("client operation failed: {error}"))
            }
            other => self.fail(format!(
                "NeedLlm requirement cannot accept a `{}` result",
                other.tag()
            )),
        }
    }

    /// Folds a complete assistant response into the pending turn.
    fn fold_llm_response(&mut self, step_id: StepId, response: Response) -> StepOutcome {
        let Some(assistant_message_id) = self.pending_assistant_message_id else {
            return self.fail("missing in-flight assistant message id for the LLM response");
        };

        if let Err(error) = self
            .state
            .conversation_mut()
            .start_assistant_response(response)
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        let finish = match self
            .state
            .conversation_mut()
            .finish_assistant(assistant_message_id)
        {
            Ok(finish) => finish,
            Err(error) => return self.fail(format!("conversation operation failed: {error}")),
        };

        match finish {
            AssistantFinish::ReadyToCommit => self.commit_text_turn(step_id),
            AssistantFinish::RequiresToolCallMappings => {
                self.fail("tool orchestration is not implemented until M2-4")
            }
        }
    }

    /// Commits a tool-free turn and emits its step-boundary notification.
    fn commit_text_turn(&mut self, step_id: StepId) -> StepOutcome {
        if let Err(error) = self
            .state
            .conversation_mut()
            .commit_pending(TurnMeta::default())
        {
            return self.fail(format!("conversation operation failed: {error}"));
        }

        let boundary = self.state.conversation().head();

        if let Err(error) = self
            .state
            .transition_cursor(LoopCursor::done(LoopDoneReason::Completed))
        {
            return self.fail(format!("cursor transition failed: {error}"));
        }

        self.pending_assistant_message_id = None;

        let notification = Notification::StepBoundary(StepBoundary::new(step_id, boundary, None));
        StepOutcome::new(vec![notification], Vec::new(), true)
    }

    /// Discards any dangling pending turn and parks the machine on a classified
    /// error cursor. `step` cannot return `Result`, so runtime failures during a
    /// step surface as an [`LoopCursor::Error`] with a quiescent outcome.
    fn fail(&mut self, message: impl Into<String>) -> StepOutcome {
        if self.state.conversation().pending().is_some() {
            let _ = self
                .state
                .conversation_mut()
                .cancel_pending(CancelDisposition::DiscardTurn);
        }
        self.pending_assistant_message_id = None;
        if let Ok(cursor) = LoopCursor::error(message) {
            let _ = self.state.transition_cursor(cursor);
        }
        StepOutcome::new(Vec::new(), Vec::new(), true)
    }
}

impl AgentMachine for DefaultAgentMachine {
    fn step(&mut self, input: StepInput) -> StepOutcome {
        match input {
            StepInput::External(AgentInput::UserMessage(user)) => self.begin_user_turn(user),
            StepInput::External(AgentInput::Pivot(_)) => {
                self.fail("pivot injection is implemented in M4")
            }
            // Legacy queued-pivot and opaque cursor-resume inputs are not part of
            // the sans-io contract; a driver feeds `StepInput::Resume` instead.
            StepInput::External(_) => self.fail(
                "legacy queued-pivot and cursor-resume inputs are not supported by the \
                 sans-io machine; feed StepInput::Resume with a requirement result",
            ),
            StepInput::Resume(resolution) => self.resume(resolution),
            StepInput::Abandon(_) => self.fail("abandon/cancel is implemented in M4"),
        }
    }

    fn cursor(&self) -> &LoopCursor {
        self.state.loop_cursor()
    }
}

#[cfg(test)]
mod tests {
    use super::DefaultAgentMachine;
    use crate::{
        agent::{
            AgentInput, AgentMachine, AgentSpec, AgentState, InteractionResponse, LlmStepMode,
            LoopCursor, LoopCursorKind, LoopPolicy, ModelRef, Notification, RequirementError,
            RequirementId, RequirementIds, RequirementKind, RequirementKindTag,
            RequirementResolution, RequirementResult, StepId, StepInput, ToolFailurePolicy,
            ToolSetRef, WorktreeRef,
        },
        client::{ClientError, Response},
        conversation::{Conversation, ConversationConfig, MessageId, TurnId},
        model::{
            content::ContentBlock,
            message::{Message, Role},
            normalized::StopReason,
            usage::Usage,
        },
    };
    use serde_json::{Map, json};
    use std::{num::NonZeroU32, sync::Arc};

    #[derive(Debug)]
    struct FixedRequirementIds(RequirementId);

    impl RequirementIds for FixedRequirementIds {
        fn next_requirement_id(
            &self,
            _kind_tag: RequirementKindTag,
        ) -> Result<RequirementId, RequirementError> {
            Ok(self.0)
        }
    }

    fn nz(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("non-zero test value")
    }

    fn agent_id() -> crate::agent::AgentId {
        "018f0d9c-7b6a-7c12-8f31-123456789001"
            .parse()
            .expect("agent id")
    }

    fn tool_set_id() -> crate::agent::ToolSetId {
        "018f0d9c-7b6a-7c12-8f31-123456789002"
            .parse()
            .expect("tool set id")
    }

    fn conversation_id() -> crate::conversation::ConversationId {
        "018f0d9c-7b6a-7c12-8f31-123456789004"
            .parse()
            .expect("conversation id")
    }

    fn turn_id() -> TurnId {
        "018f0d9c-7b6a-7c12-8f31-123456789005"
            .parse()
            .expect("turn id")
    }

    fn user_message_id() -> MessageId {
        "018f0d9c-7b6a-7c12-8f31-123456789006"
            .parse()
            .expect("user message id")
    }

    fn assistant_message_id() -> MessageId {
        "018f0d9c-7b6a-7c12-8f31-123456789007"
            .parse()
            .expect("assistant message id")
    }

    fn step_id() -> StepId {
        "018f0d9c-7b6a-7c12-8f31-123456789008"
            .parse()
            .expect("step id")
    }

    fn requirement_id() -> RequirementId {
        RequirementId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a1").expect("requirement id")
    }

    fn other_requirement_id() -> RequirementId {
        RequirementId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a2").expect("requirement id")
    }

    fn spec() -> AgentSpec {
        AgentSpec::new(
            agent_id(),
            WorktreeRef::new("/repo/agent-lib"),
            Some("Spec fallback system.".to_owned()),
            ToolSetRef::new(tool_set_id(), Vec::new()),
            ModelRef::new("gpt-5.5", nz(512), Some(0.1), None),
            LoopPolicy::new(nz(8), nz(1), ToolFailurePolicy::ReturnErrorToModel),
        )
    }

    fn state() -> AgentState {
        AgentState::new(
            spec(),
            Conversation::new(
                conversation_id(),
                ConversationConfig::new(Some("Conversation system.".to_owned())),
            ),
        )
    }

    fn machine(mode: LlmStepMode) -> DefaultAgentMachine {
        DefaultAgentMachine::new(
            state(),
            mode,
            Arc::new(FixedRequirementIds(requirement_id())),
        )
    }

    fn user_message(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
                extra: Map::new(),
            }],
        }
    }

    fn user_input() -> AgentInput {
        AgentInput::user_message(
            turn_id(),
            user_message_id(),
            user_message("hello"),
            assistant_message_id(),
            step_id(),
        )
        .expect("valid user input")
    }

    fn text_response(text: &str) -> Response {
        Response {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: text.to_owned(),
                    extra: Map::new(),
                }],
            },
            usage: Usage::default(),
            stop_reason: StopReason::normalize("end_turn"),
            extra: Map::new(),
        }
    }

    fn tool_use_response() -> Response {
        Response {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call-weather".to_owned(),
                    name: "get_weather".to_owned(),
                    input: json!({ "city": "Shanghai" }),
                    extra: Map::new(),
                }],
            },
            usage: Usage::default(),
            stop_reason: StopReason::normalize("tool_use"),
            extra: Map::new(),
        }
    }

    fn assert_text(message: &Message, expected: &str) {
        match message.content.as_slice() {
            [ContentBlock::Text { text, .. }] => assert_eq!(text, expected),
            other => panic!("expected a single text block, got {other:?}"),
        }
    }

    /// Drives the machine from Idle to a blocked `StreamingStep` and returns the
    /// emitted `NeedLlm` requirement id.
    fn park_on_need_llm(machine: &mut DefaultAgentMachine) -> RequirementId {
        let outcome = machine.step(StepInput::external(user_input()));
        assert!(outcome.is_quiescent());
        assert_eq!(outcome.requirements.len(), 1);
        assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
        outcome.requirements[0].id
    }

    #[test]
    fn user_message_emits_need_llm_and_parks_on_streaming_step() {
        let mut machine = machine(LlmStepMode::NonStreaming);

        let outcome = machine.step(StepInput::external(user_input()));

        assert!(outcome.is_quiescent());
        assert!(outcome.notifications.is_empty());
        assert_eq!(outcome.requirements.len(), 1);

        let requirement = &outcome.requirements[0];
        assert_eq!(requirement.id, requirement_id());
        assert!(requirement.origin.is_root());

        let RequirementKind::NeedLlm { request, mode } = &requirement.kind else {
            panic!("text turn must emit NeedLlm, got {:?}", requirement.kind);
        };
        assert_eq!(*mode, LlmStepMode::NonStreaming);
        assert!(!request.stream);
        assert_eq!(request.model, "gpt-5.5");
        assert_eq!(request.max_tokens, 512);
        assert_eq!(request.messages.len(), 1);
        assert_text(&request.messages[0], "hello");

        assert_eq!(machine.cursor().kind(), LoopCursorKind::StreamingStep);
        assert_eq!(
            machine.cursor().pending_requirement_ids(),
            vec![requirement_id()]
        );
        assert!(machine.state().conversation().pending().is_some());
    }

    #[test]
    fn streaming_mode_requests_stream_transport() {
        let mut machine = machine(LlmStepMode::Streaming);

        let outcome = machine.step(StepInput::external(user_input()));

        let RequirementKind::NeedLlm { request, mode } = &outcome.requirements[0].kind else {
            panic!("expected NeedLlm");
        };
        assert_eq!(*mode, LlmStepMode::Streaming);
        assert!(request.stream);
    }

    #[test]
    fn llm_text_response_commits_turn_and_emits_step_boundary() {
        let mut machine = machine(LlmStepMode::NonStreaming);
        let id = park_on_need_llm(&mut machine);

        let resolution =
            RequirementResolution::new(id, RequirementResult::Llm(Ok(text_response("hi"))));
        let outcome = machine.step(StepInput::resume(resolution));

        assert!(outcome.is_quiescent());
        assert!(outcome.requirements.is_empty());
        assert_eq!(outcome.notifications.len(), 1);
        let Notification::StepBoundary(boundary) = &outcome.notifications[0] else {
            panic!("expected a step-boundary notification");
        };
        assert_eq!(boundary.step_id(), step_id());
        assert_eq!(boundary.boundary().turn_count(), 1);

        assert_eq!(machine.cursor().kind(), LoopCursorKind::Done);
        assert!(machine.cursor().pending_requirement_ids().is_empty());

        let conversation = machine.state().conversation();
        assert!(conversation.pending().is_none());
        assert_eq!(conversation.turns().len(), 1);
        let turn = &conversation.turns()[0];
        assert_eq!(turn.messages().len(), 2);
        assert_text(turn.messages()[0].payload(), "hello");
        assert_text(turn.messages()[1].payload(), "hi");
    }

    #[test]
    fn llm_client_error_moves_cursor_to_error_and_discards_pending() {
        let mut machine = machine(LlmStepMode::NonStreaming);
        let id = park_on_need_llm(&mut machine);

        let resolution = RequirementResolution::new(
            id,
            RequirementResult::Llm(Err(ClientError::Other("boom".to_owned()))),
        );
        let outcome = machine.step(StepInput::resume(resolution));

        assert!(outcome.is_quiescent());
        assert!(outcome.requirements.is_empty());
        assert!(outcome.notifications.is_empty());

        let LoopCursor::Error(error) = machine.cursor() else {
            panic!("client error must park on the error cursor");
        };
        assert!(error.message().contains("boom"));

        let conversation = machine.state().conversation();
        assert!(conversation.pending().is_none());
        assert!(conversation.turns().is_empty());
    }

    #[test]
    fn resume_with_mismatched_requirement_id_fails() {
        let mut machine = machine(LlmStepMode::NonStreaming);
        let _id = park_on_need_llm(&mut machine);

        let resolution = RequirementResolution::new(
            other_requirement_id(),
            RequirementResult::Llm(Ok(text_response("hi"))),
        );
        let outcome = machine.step(StepInput::resume(resolution));

        assert!(outcome.is_quiescent());
        assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
        assert!(machine.state().conversation().pending().is_none());
    }

    #[test]
    fn resume_with_wrong_result_kind_fails() {
        let mut machine = machine(LlmStepMode::NonStreaming);
        let id = park_on_need_llm(&mut machine);

        let resolution = RequirementResolution::new(
            id,
            RequirementResult::Interaction(InteractionResponse::Answer("no".to_owned())),
        );
        let outcome = machine.step(StepInput::resume(resolution));

        assert!(outcome.is_quiescent());
        let LoopCursor::Error(error) = machine.cursor() else {
            panic!("type-mismatched result must park on the error cursor");
        };
        assert!(error.message().contains("interaction"));
    }

    #[test]
    fn tool_use_response_is_rejected_until_m2_4() {
        let mut machine = machine(LlmStepMode::NonStreaming);
        let id = park_on_need_llm(&mut machine);

        let resolution =
            RequirementResolution::new(id, RequirementResult::Llm(Ok(tool_use_response())));
        let outcome = machine.step(StepInput::resume(resolution));

        assert!(outcome.is_quiescent());
        let LoopCursor::Error(error) = machine.cursor() else {
            panic!("tool-use response is not implemented until M2-4");
        };
        assert!(error.message().contains("M2-4"));

        let conversation = machine.state().conversation();
        assert!(conversation.pending().is_none());
        assert!(conversation.turns().is_empty());
    }

    #[test]
    fn resume_without_outstanding_requirement_fails() {
        let mut machine = machine(LlmStepMode::NonStreaming);

        let resolution = RequirementResolution::new(
            requirement_id(),
            RequirementResult::Llm(Ok(text_response("hi"))),
        );
        let outcome = machine.step(StepInput::resume(resolution));

        assert!(outcome.is_quiescent());
        assert_eq!(machine.cursor().kind(), LoopCursorKind::Error);
    }
}
