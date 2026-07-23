//! Opt-in integration coverage for the real OpenAI Chat/Completions endpoint,
//! covering the DeepSeek and vLLM dialects (see `docs/openai-chat-api.md` §5).
//!
//! Every test is `#[ignore]` and additionally skips itself when the required
//! environment is absent, so a default `cargo test --test integration_openai_chat`
//! run exits 0 with no network access. Point the environment at a real endpoint
//! and run with `--ignored --nocapture` to exercise the live path:
//!
//! ```text
//! DEEPSEEK_API_KEY=... cargo test --test integration_openai_chat -- --ignored --nocapture
//! VLLM_BASE_URL=http://host:port/v1 VLLM_MODEL=... cargo test --test integration_openai_chat -- --ignored --nocapture
//! ```

use agent_lib::{
    adapter::openai_chat::OpenAiChatAdapter,
    client::{AuthScheme, ChatRequest, EndpointConfig, Response},
    model::{
        content::ContentBlock,
        extras::{ProviderExtras, ProviderId},
        message::{Message, Role},
        normalized::StopReason,
        tool::{Tool, ToolStatus},
    },
    stream::{
        BlockKind, Delta, StreamEvent,
        accumulator::{Accumulator, AccumulatorError},
    },
};
use futures::TryStreamExt;
use serde_json::{Map, json};
use std::time::Duration;
use tokio::time::timeout;

/// Per-call wall-clock bound. These are ignored live-network tests run only on
/// demand, so a generous bound keeps a slow provider from looking like a hang
/// while still guaranteeing the suite cannot wedge indefinitely.
const PER_CALL_TIMEOUT: Duration = Duration::from_secs(90);

/// Holds a configured DeepSeek adapter plus the model names it should target.
struct DeepSeek {
    adapter: OpenAiChatAdapter,
    chat_model: String,
    reasoner_model: String,
}

/// Holds a configured vLLM adapter plus the served model name it should target.
struct Vllm {
    adapter: OpenAiChatAdapter,
    model: String,
}

/// Reads a non-empty environment value, or `None` when unset/blank.
fn nonempty_env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

/// Builds a reusable HTTP client tuned for live integration calls.
fn integration_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(PER_CALL_TIMEOUT)
        .build()
        .expect("build integration HTTP client")
}

/// Creates a DeepSeek adapter only when `DEEPSEEK_API_KEY` is configured. The
/// key value is never printed; only the variable name surfaces in skip output.
fn deepseek() -> Option<DeepSeek> {
    let api_key = match nonempty_env("DEEPSEEK_API_KEY") {
        Some(value) => value,
        None => {
            eprintln!("skipping: DEEPSEEK_API_KEY is not configured");
            return None;
        }
    };
    let endpoint = EndpointConfig {
        base_url: nonempty_env("DEEPSEEK_BASE_URL")
            .unwrap_or_else(|| "https://api.deepseek.com".to_owned()),
        auth: AuthScheme::Bearer(api_key),
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    };
    Some(DeepSeek {
        adapter: OpenAiChatAdapter::with_http_client(endpoint, integration_http_client()),
        chat_model: nonempty_env("DEEPSEEK_MODEL").unwrap_or_else(|| "deepseek-chat".to_owned()),
        reasoner_model: nonempty_env("DEEPSEEK_REASONER_MODEL")
            .unwrap_or_else(|| "deepseek-reasoner".to_owned()),
    })
}

/// Creates a vLLM adapter only when `VLLM_BASE_URL` is configured. Authentication
/// is optional (`AuthScheme::None` when `VLLM_API_KEY` is absent), matching
/// unauthenticated vLLM deployments (§5.2).
fn vllm() -> Option<Vllm> {
    let base_url = match nonempty_env("VLLM_BASE_URL") {
        Some(value) => value,
        None => {
            eprintln!("skipping: VLLM_BASE_URL is not configured");
            return None;
        }
    };
    let auth = match nonempty_env("VLLM_API_KEY") {
        Some(api_key) => AuthScheme::Bearer(api_key),
        None => AuthScheme::None,
    };
    let endpoint = EndpointConfig {
        base_url,
        auth,
        query_params: Vec::new(),
        extra_headers: Vec::new(),
    };
    // vLLM served-model names are deployment-specific; fall back to a placeholder
    // and hint that the operator should set VLLM_MODEL to the loaded model name.
    let model = nonempty_env("VLLM_MODEL").unwrap_or_else(|| {
        eprintln!("note: VLLM_MODEL is unset; override it with your served model name");
        "default-model".to_owned()
    });
    Some(Vllm {
        adapter: OpenAiChatAdapter::with_http_client(endpoint, integration_http_client()),
        model,
    })
}

/// Builds a provider-neutral plain-text user request.
fn text_request(model: &str, prompt: &str, stream: bool) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: prompt.to_owned(),
                extra: Map::new(),
            }],
        }],
        tools: Vec::new(),
        system: None,
        max_tokens: 128,
        temperature: None,
        stream,
        provider_extras: None,
    }
}

/// Builds a deterministic weather tool used to exercise function calling.
fn weather_tool() -> Tool {
    Tool {
        name: "get_weather".to_owned(),
        description: "Get the current weather for a city".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"],
            "additionalProperties": false
        }),
    }
}

/// Builds provider extras requesting DeepSeek thinking mode. DeepSeek reasoning
/// is driven by the `deepseek-reasoner` model; the `thinking` field is carried
/// through the extras escape hatch (§5.1) and tolerated by OpenAI-compatible
/// servers, exercising the adapter's passthrough without coupling the test to a
/// provider-specific toggle.
fn thinking_extras() -> ProviderExtras {
    ProviderExtras {
        provider: ProviderId::OpenAiChat,
        fields: Map::from_iter([("thinking".to_owned(), json!({ "type": "enabled" }))]),
    }
}

/// Merges provider extras into a request, returning the updated request.
fn with_extras(mut request: ChatRequest, extras: ProviderExtras) -> ChatRequest {
    request.provider_extras = Some(extras);
    request
}

/// Folds one already-collected event vector through the shared accumulator.
fn fold_events(events: &[StreamEvent]) -> Result<Response, AccumulatorError> {
    let mut accumulator = Accumulator::new();
    for event in events {
        accumulator.push(event.clone())?;
    }
    accumulator.finish()
}

/// Starts and fully consumes one real SSE request under the per-call limit.
async fn collect_stream(adapter: &OpenAiChatAdapter, request: ChatRequest) -> Vec<StreamEvent> {
    timeout(PER_CALL_TIMEOUT, async {
        adapter
            .chat_stream(request)
            .await
            .expect("chat_stream failed to start")
            .try_collect::<Vec<_>>()
            .await
            .expect("chat_stream failed while decoding")
    })
    .await
    .expect("streaming call exceeded the per-call timeout")
}

/// Calls the configured DeepSeek endpoint and validates normalized text + usage.
#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY (optional DEEPSEEK_BASE_URL / DEEPSEEK_MODEL)"]
async fn deepseek_non_streaming_text_returns_content_and_usage() {
    let Some(deepseek) = deepseek() else {
        return;
    };

    let response = timeout(
        PER_CALL_TIMEOUT,
        deepseek.adapter.chat(text_request(
            &deepseek.chat_model,
            "Say hi in exactly two words.",
            false,
        )),
    )
    .await
    .expect("DeepSeek non-streaming call exceeded the per-call timeout")
    .expect("DeepSeek non-streaming call failed");

    assert!(
        response
            .message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text, .. } if !text.is_empty()))
    );
    assert_eq!(response.message.role, Role::Assistant);
    assert!(response.usage.input > 0, "usage should report input tokens");
    assert!(
        response.usage.output > 0,
        "usage should report output tokens"
    );
}

/// Calls the real DeepSeek streaming endpoint and validates the text event
/// sequence plus the shared accumulator result.
#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY (optional DEEPSEEK_BASE_URL / DEEPSEEK_MODEL)"]
async fn deepseek_streaming_text_yields_text_delta_and_usage() {
    let Some(deepseek) = deepseek() else {
        return;
    };

    let events = collect_stream(
        &deepseek.adapter,
        text_request(&deepseek.chat_model, "Reply with exactly: hi there", true),
    )
    .await;

    assert!(matches!(
        events.first(),
        Some(StreamEvent::MessageStart {
            role: Role::Assistant
        })
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::BlockStart {
            kind: BlockKind::Text,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::BlockDelta {
            delta: Delta::Text(text),
            ..
        } if !text.is_empty()
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Usage(usage) if usage.input > 0 && usage.output > 0
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::MessageStop { .. }))
    );

    let response = fold_events(&events).expect("fold DeepSeek text stream");
    assert_eq!(response.message.role, Role::Assistant);
    assert!(response.message.content.iter().any(|block| matches!(
        block,
        ContentBlock::Text { text, .. } if !text.is_empty()
    )));
    assert!(response.usage.output > 0);
}

/// Calls the DeepSeek reasoner model and validates that `reasoning_content`
/// normalizes into a `Thinking` content block (§4.3 / §5.1).
#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY (optional DEEPSEEK_BASE_URL / DEEPSEEK_MODEL)"]
async fn deepseek_thinking_mode_returns_reasoning_block() {
    let Some(deepseek) = deepseek() else {
        return;
    };

    let request = with_extras(
        text_request(
            &deepseek.reasoner_model,
            "Briefly reason about 7 * 8, then state the answer.",
            false,
        ),
        thinking_extras(),
    );

    let response = timeout(PER_CALL_TIMEOUT, deepseek.adapter.chat(request))
        .await
        .expect("DeepSeek thinking call exceeded the per-call timeout")
        .expect("DeepSeek thinking call failed");

    assert_eq!(response.message.role, Role::Assistant);
    assert!(
        response
            .message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Thinking { text, .. } if !text.is_empty())),
        "reasoner response should carry a non-empty Thinking block"
    );
}

/// Exercises the §5.1 reasoning-replay rule: in thinking mode with a tool call,
/// the assistant `reasoning_content` must be replayed on subsequent turns or the
/// API returns 400. The adapter's request side replays `Thinking` as
/// `reasoning_content` automatically, so the second round must succeed.
#[tokio::test]
#[ignore = "requires DEEPSEEK_API_KEY (optional DEEPSEEK_BASE_URL / DEEPSEEK_MODEL)"]
async fn deepseek_thinking_multiturn_with_tool_call_avoids_400() {
    let Some(deepseek) = deepseek() else {
        return;
    };

    // Round 1: elicit a weather tool call from the reasoner model. Its response
    // must carry both a `Thinking` block (reasoning_content) and a `ToolUse`.
    // DeepSeek thinking mode rejects the explicit `tool_choice` constraint
    // (`Thinking mode does not support this tool_choice`), so a directive system
    // prompt plus an explicit user request drives the call instead.
    let mut round1 = text_request(
        &deepseek.reasoner_model,
        "What is the weather in Tokyo? Call the get_weather tool for it.",
        false,
    );
    round1.system = Some(
        "You are a weather assistant. For any question about current weather or \
         temperature you MUST call the get_weather tool before answering, and \
         never answer such questions from your own knowledge."
            .to_owned(),
    );
    round1.tools = vec![weather_tool()];
    round1.max_tokens = 512;
    round1.provider_extras = Some(thinking_extras());

    let first = timeout(PER_CALL_TIMEOUT, deepseek.adapter.chat(round1))
        .await
        .expect("DeepSeek round 1 exceeded the per-call timeout")
        .expect("DeepSeek round 1 failed");
    assert_eq!(
        *first.stop_reason.value(),
        StopReason::ToolUse,
        "round 1 should stop on a tool call"
    );
    assert!(
        first
            .message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Thinking { text, .. } if !text.is_empty())),
        "round 1 should carry reasoning_content to replay"
    );
    let tool_call_id = first
        .message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::ToolUse { id, name, .. } if name == "get_weather" => Some(id.clone()),
            _ => None,
        })
        .expect("round 1 should produce a get_weather tool call");

    // Round 2: replay the full history — the round-1 assistant message keeps its
    // `Thinking` + `ToolUse` blocks, which the adapter serializes back to
    // `reasoning_content` + `tool_calls`; then a tool result and a follow-up.
    // §5.1 says omitting `reasoning_content` here triggers a 400, so a clean
    // response proves the adapter's automatic replay satisfies the rule.
    let round2 = ChatRequest {
        model: deepseek.reasoner_model.clone(),
        messages: vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "What is the weather in Tokyo? Use the get_weather tool.".to_owned(),
                    extra: Map::new(),
                }],
            },
            Message {
                role: Role::Assistant,
                content: first.message.content.clone(),
            },
            Message {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: tool_call_id,
                    content: vec![ContentBlock::Text {
                        text: "{\"temperature\": 18, \"unit\": \"celsius\"}".to_owned(),
                        extra: Map::new(),
                    }],
                    status: ToolStatus::Ok,
                    extra: Map::new(),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Using that weather, reply in one short sentence.".to_owned(),
                    extra: Map::new(),
                }],
            },
        ],
        tools: vec![weather_tool()],
        system: None,
        max_tokens: 1024,
        temperature: None,
        stream: false,
        provider_extras: Some(thinking_extras()),
    };

    let second = timeout(PER_CALL_TIMEOUT, deepseek.adapter.chat(round2))
        .await
        .expect("DeepSeek round 2 exceeded the per-call timeout")
        .expect(
            "DeepSeek round 2 should not return 400 — reasoning_content replayed \
             automatically by the adapter (§5.1)",
        );
    assert_eq!(second.message.role, Role::Assistant);
    assert!(
        second
            .message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text, .. } if !text.is_empty())),
        "round 2 should produce a final answer"
    );
}

/// Calls the vLLM endpoint non-streaming and validates normalized text + usage.
#[tokio::test]
#[ignore = "requires VLLM_BASE_URL (optional VLLM_API_KEY / VLLM_MODEL)"]
async fn vllm_non_streaming_text_smoke() {
    let Some(vllm) = vllm() else {
        return;
    };

    let response = timeout(
        PER_CALL_TIMEOUT,
        vllm.adapter.chat(text_request(
            &vllm.model,
            "Say hi in exactly two words.",
            false,
        )),
    )
    .await
    .expect("vLLM non-streaming call exceeded the per-call timeout")
    .expect("vLLM non-streaming call failed");

    assert!(
        response
            .message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text, .. } if !text.is_empty()))
    );
    assert_eq!(response.message.role, Role::Assistant);
    assert!(response.usage.input > 0, "usage should report input tokens");
}

/// Calls the vLLM streaming endpoint and validates the text event sequence. If
/// the server was started with `--reasoning-parser`, a reasoning block may also
/// appear (§5.2 待验证项); its presence is logged but not asserted.
#[tokio::test]
#[ignore = "requires VLLM_BASE_URL (optional VLLM_API_KEY / VLLM_MODEL)"]
async fn vllm_streaming_text_smoke() {
    let Some(vllm) = vllm() else {
        return;
    };

    let events = collect_stream(
        &vllm.adapter,
        text_request(&vllm.model, "Reply with exactly: hi there", true),
    )
    .await;

    assert!(matches!(
        events.first(),
        Some(StreamEvent::MessageStart {
            role: Role::Assistant
        })
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::BlockDelta {
            delta: Delta::Text(text),
            ..
        } if !text.is_empty()
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::MessageStop { .. }))
    );

    if events.iter().any(|event| {
        matches!(
            event,
            StreamEvent::BlockStart {
                kind: BlockKind::Reasoning,
                ..
            }
        )
    }) {
        eprintln!(
            "vLLM stream carried a reasoning block; the endpoint likely runs \
             --reasoning-parser and accepts reasoning_content replay (§5.2)"
        );
    }

    let response = fold_events(&events).expect("fold vLLM text stream");
    assert_eq!(response.message.role, Role::Assistant);
    assert!(response.message.content.iter().any(|block| matches!(
        block,
        ContentBlock::Text { text, .. } if !text.is_empty()
    )));
}
