use agent_lib::{
    client::Response,
    conversation::{
        Artifact, ArtifactId, ArtifactProvenance, AssistantFinish, CancelDisposition,
        CancelOutcome, CancelledToolResult, CheckedTurnRange, CompactionPlan, CompactionStep,
        Conversation, ConversationConfig, ConversationId, MessageId, StrategyRef, TokenAccounting,
        ToolCallId, ToolCallMapping, TurnId, TurnMeta,
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        tool::{ToolResponse, ToolStatus},
        usage::Usage,
    },
};
use serde_json::{Map, json};
use std::error::Error;
use uuid::Uuid;

const UUID_BASE: u128 = 0x018f_0d9c_7b6a_7c12_a600_0000_0000_0000;

fn conversation_id(seed: u128) -> ConversationId {
    ConversationId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn turn_id(seed: u128) -> TurnId {
    TurnId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn message_id(seed: u128) -> MessageId {
    MessageId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn tool_call_id(seed: u128) -> ToolCallId {
    ToolCallId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn artifact_id(seed: u128) -> ArtifactId {
    ArtifactId::new(Uuid::from_u128(UUID_BASE + seed))
}

fn text(value: impl Into<String>) -> ContentBlock {
    ContentBlock::Text {
        text: value.into(),
        extra: Map::new(),
    }
}

fn user(value: impl Into<String>) -> Message {
    Message {
        role: Role::User,
        content: vec![text(value)],
    }
}

fn assistant_response(
    content: Vec<ContentBlock>,
    stop_reason: &'static str,
    request_id: &'static str,
) -> Response {
    Response {
        message: Message {
            role: Role::Assistant,
            content,
        },
        usage: Usage {
            input: 8,
            output: 4,
            ..Usage::default()
        },
        stop_reason: StopReason::normalize(stop_reason),
        extra: Map::from_iter([("request_id".to_owned(), json!(request_id))]),
    }
}

fn tool_use(provider_call_id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: provider_call_id.to_owned(),
        name: "lookup".to_owned(),
        input: json!({ "key": provider_call_id }),
        extra: Map::new(),
    }
}

fn tool_response(provider_call_id: &str, value: &str, status: ToolStatus) -> ToolResponse {
    ToolResponse {
        tool_call_id: provider_call_id.to_owned(),
        content: vec![text(value)],
        status,
        extra: Map::new(),
    }
}

fn commit_tool_turn(conversation: &mut Conversation) -> Result<(), Box<dyn Error>> {
    conversation.begin_turn(turn_id(10), message_id(1000), user("lookup the weather"))?;
    conversation.start_assistant_response(assistant_response(
        vec![text("I will call lookup."), tool_use("lookup-weather")],
        "tool_use",
        "first-tool-use",
    ))?;
    assert_eq!(
        conversation.finish_assistant(message_id(1001))?,
        AssistantFinish::RequiresToolCallMappings
    );

    conversation.register_tool_calls(vec![ToolCallMapping::new(
        "lookup-weather",
        tool_call_id(1),
    )])?;
    conversation.append_tool_response(
        message_id(1002),
        tool_response("lookup-weather", "weather is 21C", ToolStatus::Ok),
    )?;
    conversation.start_assistant_response(assistant_response(
        vec![text("The lookup returned 21C.")],
        "end_turn",
        "first-final",
    ))?;
    assert_eq!(
        conversation.finish_assistant(message_id(1003))?,
        AssistantFinish::ReadyToCommit
    );
    conversation.commit_pending(TurnMeta::default())?;
    Ok(())
}

fn commit_cancelled_turn(conversation: &mut Conversation) -> Result<(), Box<dyn Error>> {
    conversation.begin_turn(turn_id(20), message_id(2000), user("try the slow lookup"))?;
    conversation.start_assistant_response(assistant_response(
        vec![text("I will try the slow lookup."), tool_use("lookup-slow")],
        "tool_use",
        "slow-tool-use",
    ))?;
    assert_eq!(
        conversation.finish_assistant(message_id(2001))?,
        AssistantFinish::RequiresToolCallMappings
    );
    conversation.register_tool_calls(vec![ToolCallMapping::new("lookup-slow", tool_call_id(2))])?;

    let outcome = conversation.cancel_pending(CancelDisposition::ResumeTurn {
        cancelled_results: vec![CancelledToolResult::new(
            "lookup-slow",
            tool_call_id(2),
            message_id(2002),
        )],
    })?;
    assert_eq!(
        outcome,
        CancelOutcome::Resumed {
            turn_id: turn_id(20)
        }
    );

    conversation.start_assistant_response(assistant_response(
        vec![text(
            "The slow lookup was cancelled; continuing without it.",
        )],
        "end_turn",
        "cancel-final",
    ))?;
    assert_eq!(
        conversation.finish_assistant(message_id(2003))?,
        AssistantFinish::ReadyToCommit
    );
    conversation.commit_pending(TurnMeta::default())?;
    Ok(())
}

fn checked_range(
    conversation: &Conversation,
    start: usize,
    end: usize,
) -> Result<CheckedTurnRange, Box<dyn Error>> {
    let boundaries = conversation.valid_boundaries();
    Ok(conversation.checked_turn_range(boundaries[start], boundaries[end])?)
}

fn compact_first_turn(conversation: &mut Conversation) -> Result<(), Box<dyn Error>> {
    let range = checked_range(conversation, 0, 1)?;
    let strategy = StrategyRef::new("example-summary", "v1");
    let artifact = Artifact::new(
        artifact_id(700),
        vec![Message {
            role: Role::Assistant,
            content: vec![text("summary: weather lookup completed")],
        }],
        ArtifactProvenance::new(
            range.clone(),
            strategy.clone(),
            TokenAccounting::new(
                Usage {
                    input: 24,
                    output: 12,
                    ..Usage::default()
                },
                Usage {
                    input: 4,
                    output: 2,
                    ..Usage::default()
                },
            ),
            Map::new(),
        ),
    )?;
    let plan = CompactionPlan::new(
        conversation,
        vec![CompactionStep::raw(range, artifact.id(), strategy)],
        vec![artifact],
    );
    conversation.apply_compaction(&plan)?;
    Ok(())
}

fn view_contains_text(messages: &[Message], expected: &str) -> bool {
    messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text, .. } if text == expected))
    })
}

fn tool_status(messages: &[Message], provider_call_id: &str) -> Option<ToolStatus> {
    messages.iter().find_map(|message| {
        message.content.iter().find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                status,
                ..
            } if tool_use_id == provider_call_id => Some(*status),
            _ => None,
        })
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut conversation = Conversation::new(
        conversation_id(1),
        ConversationConfig::new(Some("Answer using audited conversation state.".to_owned())),
    );

    commit_tool_turn(&mut conversation)?;
    commit_cancelled_turn(&mut conversation)?;

    assert_eq!(conversation.turns().len(), 2);
    let raw_view = conversation.effective_view();
    assert_eq!(
        raw_view.system(),
        Some("Answer using audited conversation state.")
    );
    assert_eq!(
        tool_status(raw_view.messages(), "lookup-slow"),
        Some(ToolStatus::Cancelled)
    );

    let fork_point = conversation.valid_boundaries()[1];
    let mut child = conversation.fork_at(fork_point, conversation_id(2))?;
    assert_eq!(child.turns().len(), 1);
    assert_eq!(
        child.origin().expect("fork child records origin").parent(),
        conversation.id()
    );
    child.begin_turn(
        turn_id(30),
        message_id(3000),
        user("child branch follow-up"),
    )?;
    child.start_assistant_response(assistant_response(
        vec![text("child branch answer")],
        "end_turn",
        "child-final",
    ))?;
    assert_eq!(
        child.finish_assistant(message_id(3001))?,
        AssistantFinish::ReadyToCommit
    );
    child.commit_pending(TurnMeta::default())?;
    assert_eq!(child.turns().len(), 2);
    assert_eq!(conversation.turns().len(), 2);

    compact_first_turn(&mut conversation)?;
    let compacted_view = conversation.effective_view();
    assert!(view_contains_text(
        compacted_view.messages(),
        "summary: weather lookup completed"
    ));
    assert!(view_contains_text(
        compacted_view.messages(),
        "The slow lookup was cancelled; continuing without it."
    ));

    let snapshot = conversation.snapshot()?;
    let snapshot_json = serde_json::to_string_pretty(&snapshot)?;
    let decoded = serde_json::from_str(&snapshot_json)?;
    let restored = Conversation::restore(decoded)?;
    assert_eq!(restored.effective_view(), conversation.effective_view());
    assert_eq!(restored.raw_turns().len(), conversation.raw_turns().len());

    println!(
        "conversation_core example ok: parent_turns={}, child_turns={}, snapshot_bytes={}",
        conversation.turns().len(),
        child.turns().len(),
        snapshot_json.len()
    );
    Ok(())
}
