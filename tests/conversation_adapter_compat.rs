//! Cross-adapter request-mapper acceptance for Conversation effective views.

#[allow(dead_code, unused_imports)]
#[path = "conversation_state_machine/support.rs"]
mod support;

use agent_lib::{
    adapter::{anthropic::AnthropicAdapter, openai_resp::OpenAiRespAdapter},
    client::{AuthScheme, ChatRequest, EndpointConfig},
    conversation::{
        AssistantFinish, CancelDisposition, CancelOutcome, CancelledToolResult, Conversation,
        ConversationError, ToolCallMapping, TurnMeta,
    },
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::StopReason,
        tool::{Tool, ToolStatus},
    },
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use support::*;

fn endpoint() -> EndpointConfig {
    EndpointConfig {
        base_url: "https://example.test".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    }
}

fn request_body(request: &reqwest::Request) -> Value {
    let bytes = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .expect("request mapper buffers JSON bodies");
    serde_json::from_slice(bytes).expect("request body is valid JSON")
}

fn lookup_tool() -> Tool {
    Tool {
        name: "lookup".to_owned(),
        description: "Look up one deterministic compatibility value.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"]
        }),
    }
}

fn thinking(value: &str) -> ContentBlock {
    ContentBlock::Thinking {
        text: value.to_owned(),
        signature: Some(format!("signature-{value}")),
        extra: Map::new(),
    }
}

fn tool_use(provider_call_id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: provider_call_id.to_owned(),
        name: "lookup".to_owned(),
        input: json!({ "query": provider_call_id }),
        extra: Map::new(),
    }
}

fn chat_request_from_effective_view(conversation: &Conversation) -> ChatRequest {
    let view = conversation.effective_view();
    let (system, messages) = view.into_parts();
    ChatRequest {
        model: "compat-model".to_owned(),
        messages,
        tools: vec![lookup_tool()],
        system,
        max_tokens: 256,
        temperature: None,
        stream: false,
        provider_extras: None,
    }
}

fn canonical_tool_facts(messages: &[Message]) -> (BTreeSet<String>, BTreeMap<String, ToolStatus>) {
    let mut tool_uses = BTreeSet::new();
    let mut tool_results = BTreeMap::new();
    for message in messages {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    assert_eq!(
                        message.role,
                        Role::Assistant,
                        "tool use must remain in assistant messages"
                    );
                    assert!(tool_uses.insert(id.clone()), "duplicate tool use {id}");
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    status,
                    ..
                } => {
                    assert_eq!(
                        message.role,
                        Role::Tool,
                        "tool result must remain in tool messages"
                    );
                    assert!(
                        tool_results.insert(tool_use_id.clone(), *status).is_none(),
                        "duplicate tool result {tool_use_id}"
                    );
                }
                ContentBlock::Text { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::Thinking { .. } => {}
            }
        }
    }

    for result_id in tool_results.keys() {
        assert!(
            tool_uses.contains(result_id),
            "tool result {result_id} must have a matching tool use in the same effective view"
        );
    }
    (tool_uses, tool_results)
}

fn assert_common_effective_view_facts(conversation: &Conversation, request: &ChatRequest) {
    assert_eq!(
        request.system.as_deref(),
        conversation.effective_view().system(),
        "system prompt must be copied from EffectiveView, not encoded as a message"
    );
    assert!(
        request
            .messages
            .iter()
            .all(|message| message.role != Role::System),
        "Conversation effective_view must not emit system-role messages"
    );
    assert!(
        request.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Text { text, .. }
                        if text == "summary:compat-warmup-and-prep"
                )
            })
        }),
        "compaction artifact must be rendered into the same EffectiveView"
    );
    assert!(
        request.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Thinking { .. }))
        }),
        "raw visible suffix must retain assistant reasoning blocks"
    );

    let (tool_uses, tool_results) = canonical_tool_facts(&request.messages);
    assert_eq!(
        tool_uses,
        BTreeSet::from(["compat-cancelled".to_owned(), "compat-denied".to_owned()])
    );
    assert_eq!(
        tool_results,
        BTreeMap::from([
            ("compat-cancelled".to_owned(), ToolStatus::Cancelled),
            ("compat-denied".to_owned(), ToolStatus::Denied),
        ]),
        "Conversation facts must preserve the precise normalized result status"
    );
}

fn count_items(items: &[Value], item_type: &str) -> usize {
    items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some(item_type))
        .count()
}

fn assert_anthropic_wire(body: &Value, system: &str) {
    assert_eq!(body["system"], json!(system));
    assert_eq!(body["tools"][0]["name"], json!("lookup"));
    let messages = body["messages"].as_array().expect("Anthropic messages");
    assert!(
        messages.iter().all(|message| {
            matches!(message["role"].as_str(), Some("user") | Some("assistant"))
        })
    );
    assert!(
        messages.iter().any(|message| {
            message["content"]
                .as_array()
                .expect("Anthropic content")
                .iter()
                .any(|block| {
                    block["type"] == "text" && block["text"] == "summary:compat-warmup-and-prep"
                })
        }),
        "Anthropic wire must include compacted artifact content"
    );

    let blocks = messages
        .iter()
        .flat_map(|message| {
            message["content"]
                .as_array()
                .expect("Anthropic message content")
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(count_items(&blocks, "thinking"), 1);
    assert_eq!(count_items(&blocks, "tool_use"), 2);
    assert_eq!(count_items(&blocks, "tool_result"), 2);

    for provider_call_id in ["compat-denied", "compat-cancelled"] {
        let use_block = blocks
            .iter()
            .find(|block| block["type"] == "tool_use" && block["id"] == provider_call_id)
            .expect("Anthropic tool use keeps provider call id");
        assert_eq!(use_block["name"], json!("lookup"));

        let result = blocks
            .iter()
            .find(|block| {
                block["type"] == "tool_result" && block["tool_use_id"] == provider_call_id
            })
            .expect("Anthropic tool result keeps provider call id");
        assert!(result.get("status").is_none());
        assert_eq!(
            result["is_error"],
            json!(true),
            "Denied and Cancelled degrade only at Anthropic wire boundary"
        );
    }
}

fn assert_openai_wire(body: &Value, system: &str) {
    assert_eq!(body["instructions"], json!(system));
    assert_eq!(body["tools"][0]["name"], json!("lookup"));
    let input = body["input"].as_array().expect("OpenAI input items");
    assert!(input.iter().all(|item| item["role"] != "system"));
    assert_eq!(count_items(input, "reasoning"), 1);
    assert_eq!(count_items(input, "function_call"), 2);
    assert_eq!(count_items(input, "function_call_output"), 2);
    assert!(
        input.iter().any(|item| {
            item["role"] == "assistant"
                && item["content"]
                    .as_array()
                    .expect("OpenAI assistant content")
                    .iter()
                    .any(|block| {
                        block["type"] == "output_text"
                            && block["text"] == "summary:compat-warmup-and-prep"
                    })
        }),
        "OpenAI wire must include compacted artifact content"
    );

    for provider_call_id in ["compat-denied", "compat-cancelled"] {
        let call = input
            .iter()
            .find(|item| item["type"] == "function_call" && item["call_id"] == provider_call_id)
            .expect("OpenAI function call keeps provider call id");
        assert_eq!(call["name"], json!("lookup"));

        let output = input
            .iter()
            .find(|item| {
                item["type"] == "function_call_output" && item["call_id"] == provider_call_id
            })
            .expect("OpenAI function output keeps provider call id");
        assert_eq!(
            output["status"],
            json!("incomplete"),
            "Denied and Cancelled degrade only at OpenAI wire boundary"
        );
    }
}

fn build_compatibility_conversation() -> Conversation {
    let mut conversation = conversation(40);
    commit_text_turn(&mut conversation, 4_001, "compat-warmup");
    commit_tool_turn(
        &mut conversation,
        4_002,
        "compat-prep-call",
        400_200,
        "compat-prep",
    );
    apply_raw_compaction(
        &mut conversation,
        0,
        2,
        400_300,
        "adapter-compat",
        "summary:compat-warmup-and-prep",
    );

    begin(&mut conversation, 4_003, "compat-status");
    assert_eq!(
        finish_complete_response(
            &mut conversation,
            assistant_response(
                vec![
                    text("assistant:compat-parallel-status"),
                    thinking("adapter-compat-reasoning"),
                    tool_use("compat-denied"),
                    tool_use("compat-cancelled"),
                ],
                usage(8, 3),
                StopReason::ToolUse,
                "compat-parallel-status",
            ),
            400_301,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    conversation
        .register_tool_calls(vec![
            ToolCallMapping::new("compat-denied", call_id(400_302)),
            ToolCallMapping::new("compat-cancelled", call_id(400_303)),
        ])
        .expect("register compatibility tool mappings");
    conversation
        .append_tool_response(
            message_id(400_304),
            tool_response("compat-denied", "denied by policy", ToolStatus::Denied),
        )
        .expect("append denied result");
    assert_eq!(
        conversation
            .cancel_pending(CancelDisposition::ResumeTurn {
                cancelled_results: vec![CancelledToolResult::new(
                    "compat-cancelled",
                    call_id(400_303),
                    message_id(400_305),
                )],
            })
            .expect("synthesize cancelled result"),
        CancelOutcome::Resumed {
            turn_id: turn_id(4_003)
        }
    );
    assert_eq!(
        finish_complete_response(
            &mut conversation,
            assistant_response(
                vec![text("assistant:compat-final")],
                usage(2, 1),
                StopReason::EndTurn,
                "compat-final",
            ),
            400_306,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit compatibility turn");
    assert_state_machine_invariants("adapter compatibility fixture", &conversation);
    conversation
}

#[test]
fn same_effective_view_builds_anthropic_and_openai_requests() {
    let conversation = build_compatibility_conversation();
    let request = chat_request_from_effective_view(&conversation);
    let original_messages = request.messages.clone();
    assert_common_effective_view_facts(&conversation, &request);

    let anthropic = AnthropicAdapter::new(endpoint())
        .build_request(&request)
        .expect("Anthropic mapper accepts the Conversation effective view");
    let openai = OpenAiRespAdapter::new(endpoint())
        .build_request(&request)
        .expect("OpenAI Responses mapper accepts the Conversation effective view");

    assert_eq!(
        request.messages, original_messages,
        "adapter wire mapping must not mutate the shared normalized source view"
    );
    assert_anthropic_wire(
        &request_body(&anthropic),
        request.system.as_deref().expect("system prompt"),
    );
    assert_openai_wire(
        &request_body(&openai),
        request.system.as_deref().expect("system prompt"),
    );
}

#[test]
fn dangling_tool_calls_cannot_reach_committed_effective_view() {
    let mut conversation = conversation(41);
    begin(&mut conversation, 4_101, "dangling-tool");
    assert_eq!(
        finish_complete_response(
            &mut conversation,
            assistant_response(
                vec![text("assistant:wants-tool"), tool_use("dangling-call")],
                usage(3, 1),
                StopReason::ToolUse,
                "dangling-call",
            ),
            410_101,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    let before_failed_commit = runtime_state(&conversation);
    let error = conversation
        .commit_pending(TurnMeta::default())
        .expect_err("dangling tool call cannot be committed");
    assert!(
        matches!(error, ConversationError::PendingTurn(_)),
        "dangling commit must fail in Conversation before any adapter sees it: {error:?}"
    );
    assert_eq!(
        runtime_state(&conversation),
        before_failed_commit,
        "failed commit must leave pending and committed state unchanged"
    );
    assert!(
        conversation.effective_view().is_empty(),
        "committed EffectiveView must not include unpaired pending tool calls"
    );

    conversation
        .cancel_pending(CancelDisposition::DiscardTurn)
        .expect("discard invalid pending turn");
    assert_can_commit_followup("after dangling rejection", &mut conversation, 4_102);
}
