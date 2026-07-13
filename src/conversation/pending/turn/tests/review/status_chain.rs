//! Four-state tool-result audit from Conversation facts to provider wire.

use super::super::*;
use crate::{
    adapter::{anthropic::AnthropicAdapter, openai_resp::OpenAiRespAdapter},
    client::{AuthScheme, ChatRequest, EndpointConfig},
    conversation::{CANCELLED_TOOL_RESULT_TEXT, CancelDisposition, CancelledToolResult, TurnMeta},
    model::tool::Tool,
};
use serde_json::Value;

/// Creates transport configuration suitable for local request serialization only.
fn review_endpoint() -> EndpointConfig {
    EndpointConfig {
        base_url: "https://example.test".to_owned(),
        auth: AuthScheme::None,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    }
}

/// Decodes the buffered JSON body produced without sending an HTTP request.
fn request_body(request: &reqwest::Request) -> Value {
    let bytes = request
        .body()
        .and_then(reqwest::Body::as_bytes)
        .expect("review request has a buffered JSON body");
    serde_json::from_slice(bytes).expect("review request body is valid JSON")
}

/// Reads normalized provider-call/status pairs from complete tool-role messages.
fn normalized_result_statuses(messages: &[ConversationMessage]) -> Vec<(String, ToolStatus)> {
    messages
        .iter()
        .filter(|message| message.payload().role == Role::Tool)
        .map(|message| {
            let [
                ContentBlock::ToolResult {
                    tool_use_id,
                    status,
                    extra,
                    ..
                },
            ] = message.payload().content.as_slice()
            else {
                panic!("canonical tool message contains exactly one result block")
            };
            assert!(!extra.contains_key("status"));
            assert!(!extra.contains_key("is_error"));
            (tool_use_id.clone(), *status)
        })
        .collect()
}

/// Audits the four-state fact from tool execution through both wire boundaries.
#[test]
fn four_tool_statuses_survive_pending_commit_and_adapter_degradation() {
    let status_specs = [
        ("review-ok", ToolStatus::Ok),
        ("review-error", ToolStatus::Error),
        ("review-denied", ToolStatus::Denied),
        ("review-cancelled", ToolStatus::Cancelled),
    ];
    let mut conversation = conversation();
    begin(&mut conversation, 5_000, 5_001);
    assert_eq!(
        freeze_response(
            &mut conversation,
            assistant_response(
                status_specs
                    .iter()
                    .map(|(provider_call_id, _)| tool_use(provider_call_id))
                    .collect(),
                8,
                4,
                StopReason::ToolUse,
                "req-four-statuses",
            ),
            5_002,
        ),
        AssistantFinish::RequiresToolCallMappings
    );
    conversation
        .register_tool_calls(
            status_specs
                .iter()
                .enumerate()
                .map(|(index, (provider_call_id, _))| {
                    mapping(provider_call_id, 5_100 + index as u128)
                })
                .collect(),
        )
        .expect("map all four review calls");

    for (index, (provider_call_id, status)) in status_specs[..3].iter().enumerate() {
        conversation
            .append_tool_response(
                message_id(5_003 + index as u128),
                ToolResponse {
                    tool_call_id: (*provider_call_id).to_owned(),
                    content: vec![text(&format!("{provider_call_id} result"))],
                    status: *status,
                    extra: Map::from_iter([(
                        "provider_trace".to_owned(),
                        serde_json::json!(format!("trace-{index}")),
                    )]),
                },
            )
            .expect("append a complete non-cancelled result");
    }
    conversation
        .cancel_pending(CancelDisposition::ResumeTurn {
            cancelled_results: vec![CancelledToolResult::new(
                status_specs[3].0,
                call_id(5_103),
                message_id(5_006),
            )],
        })
        .expect("synthesize the fourth, cancelled result");

    let pending = conversation.pending().expect("resumed four-status turn");
    assert_eq!(pending.phase(), PendingTurnPhase::AwaitingAssistant);
    assert_eq!(pending.open_calls().count(), 0);
    assert_eq!(
        normalized_result_statuses(pending.messages()),
        status_specs
            .iter()
            .map(|(provider_call_id, status)| ((*provider_call_id).to_owned(), *status))
            .collect::<Vec<_>>()
    );
    let ContentBlock::ToolResult {
        content,
        status: ToolStatus::Cancelled,
        ..
    } = &pending.messages()[5].payload().content[0]
    else {
        panic!("the final pending result is the synthetic cancellation")
    };
    assert_eq!(content, &vec![text(CANCELLED_TOOL_RESULT_TEXT)]);

    assert_eq!(
        freeze_response(
            &mut conversation,
            assistant_response(
                vec![text("all outcomes observed")],
                2,
                1,
                StopReason::EndTurn,
                "req-four-status-final",
            ),
            5_007,
        ),
        AssistantFinish::ReadyToCommit
    );
    conversation
        .commit_pending(TurnMeta::default())
        .expect("commit the four-status turn through the sole validator");
    let turn = &conversation.turns()[0];
    assert_eq!(
        normalized_result_statuses(turn.messages()),
        status_specs
            .iter()
            .map(|(provider_call_id, status)| ((*provider_call_id).to_owned(), *status))
            .collect::<Vec<_>>()
    );
    assert_eq!(turn.pairings().len(), status_specs.len());

    for message in turn
        .messages()
        .iter()
        .filter(|message| message.payload().role == Role::Tool)
    {
        let ContentBlock::ToolResult { status, .. } = &message.payload().content[0] else {
            unreachable!("canonical tool message contains a result")
        };
        let encoded = serde_json::to_value(&message.payload().content[0])
            .expect("serialize normalized result fact");
        let expected = match status {
            ToolStatus::Ok => "ok",
            ToolStatus::Error => "error",
            ToolStatus::Denied => "denied",
            ToolStatus::Cancelled => "cancelled",
        };
        assert_eq!(encoded["status"], serde_json::json!(expected));
        assert!(encoded.get("is_error").is_none());
    }

    let messages = turn
        .messages()
        .iter()
        .map(|message| message.payload().clone())
        .collect::<Vec<_>>();
    let original_messages = messages.clone();
    let request = ChatRequest {
        model: "review-model".to_owned(),
        messages,
        tools: vec![Tool {
            name: "lookup".to_owned(),
            description: "Look up one review value.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }),
        }],
        system: conversation.config().system().map(str::to_owned),
        max_tokens: 128,
        temperature: None,
        stream: false,
        provider_extras: None,
    };
    let anthropic = AnthropicAdapter::new(review_endpoint())
        .build_request(&request)
        .expect("Anthropic mapper accepts all four normalized facts");
    let openai = OpenAiRespAdapter::new(review_endpoint())
        .build_request(&request)
        .expect("OpenAI mapper accepts all four normalized facts");
    assert_eq!(
        request.messages, original_messages,
        "wire degradation cannot mutate the normalized source facts"
    );

    let anthropic_body = request_body(&anthropic);
    let anthropic_results = anthropic_body["messages"]
        .as_array()
        .expect("Anthropic messages array")
        .iter()
        .flat_map(|message| {
            message["content"]
                .as_array()
                .expect("Anthropic content array")
        })
        .filter(|block| block["type"] == "tool_result")
        .collect::<Vec<_>>();
    assert_eq!(anthropic_results.len(), status_specs.len());
    for (provider_call_id, status) in status_specs {
        let result = anthropic_results
            .iter()
            .find(|result| result["tool_use_id"] == provider_call_id)
            .expect("Anthropic result retains provider call id");
        assert!(result.get("status").is_none());
        match status {
            ToolStatus::Ok => assert!(result.get("is_error").is_none()),
            ToolStatus::Error | ToolStatus::Denied | ToolStatus::Cancelled => {
                assert_eq!(result["is_error"], serde_json::json!(true));
            }
        }
    }

    let openai_body = request_body(&openai);
    let openai_results = openai_body["input"]
        .as_array()
        .expect("OpenAI input array")
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect::<Vec<_>>();
    assert_eq!(openai_results.len(), status_specs.len());
    for (provider_call_id, status) in status_specs {
        let result = openai_results
            .iter()
            .find(|result| result["call_id"] == provider_call_id)
            .expect("OpenAI result retains provider call id");
        let expected = match status {
            ToolStatus::Ok => "completed",
            ToolStatus::Error | ToolStatus::Denied | ToolStatus::Cancelled => "incomplete",
        };
        assert_eq!(result["status"], serde_json::json!(expected));
    }
    assert_eq!(
        normalized_result_statuses(turn.messages()),
        [
            ("review-ok".to_owned(), ToolStatus::Ok),
            ("review-error".to_owned(), ToolStatus::Error),
            ("review-denied".to_owned(), ToolStatus::Denied),
            ("review-cancelled".to_owned(), ToolStatus::Cancelled),
        ],
        "provider wire mapping leaves closed history unchanged"
    );
}
