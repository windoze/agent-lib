use super::{
    ExternalAgentError, ExternalAgentEvent, ExternalAgentOutput, ExternalArtifactKind,
    ExternalArtifactRef, ExternalCapability, ExternalObservedEvent, ExternalPermissionMode,
    ExternalRuntimeKind, ExternalSessionInput, ExternalSessionPolicy, ExternalSessionRef,
    ExternalSessionRequest, ExternalSessionResult, ExternalStreamPolicy, ExternalSubagentOutput,
    ExternalSubagentRequest, ExternalSubagentRequestId, ExternalToolBatchId, ExternalToolCall,
    ExternalToolResult, WorktreeIsolation, collect_file_patch_artifacts,
    collect_file_patch_artifacts_from_observed,
};
use crate::{
    agent::{
        AgentId, AgentSpecRef, StepId, SubagentOutput, interaction::Interaction, spec::WorktreeRef,
        tool::ToolRuntimeError,
    },
    model::{
        content::ContentBlock,
        tool::{Tool, ToolCall, ToolResponse, ToolStatus},
        usage::Usage,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, json};
use std::fmt::Debug;

fn agent_id() -> AgentId {
    "018f0d9c-7b6a-7c12-8f31-1234567890c1"
        .parse()
        .expect("agent id")
}

fn step_id() -> StepId {
    "018f0d9c-7b6a-7c12-8f31-1234567890c2"
        .parse()
        .expect("step id")
}

fn sample_tool() -> Tool {
    Tool {
        name: "apply_patch".to_owned(),
        description: "Apply a unified diff to the worktree.".to_owned(),
        input_schema: json!({ "type": "object" }),
    }
}

fn session_ref() -> ExternalSessionRef {
    ExternalSessionRef {
        runtime: ExternalRuntimeKind::ClaudeCode,
        session_id: Some("sess-42".to_owned()),
        transcript_ref: Some("transcript://42".to_owned()),
        resume_token: Some("resume-token".to_owned()),
        last_event_seq: Some(7),
    }
}

fn sample_request() -> ExternalSessionRequest {
    ExternalSessionRequest {
        agent_id: agent_id(),
        runtime: ExternalRuntimeKind::Custom("bespoke-cli".to_owned()),
        worktree: WorktreeRef::new("/repo/agent-lib"),
        session_dir: None,
        session: Some(session_ref()),
        input: ExternalSessionInput::Start {
            prompt: "Refactor the parser.".to_owned(),
        },
        tools: vec![sample_tool()],
        policy: ExternalSessionPolicy {
            permission_mode: ExternalPermissionMode::AcceptEdits,
            isolation: WorktreeIsolation::EphemeralGitWorktree,
            max_turns: Some(16),
            stream_events: ExternalStreamPolicy::Buffered,
        },
    }
}

fn sample_observations() -> Vec<ExternalObservedEvent> {
    ExternalObservedEvent::unsequenced_for_tests(vec![
        ExternalAgentEvent::SessionStarted {
            session_id: Some("sess-42".to_owned()),
        },
        ExternalAgentEvent::TextDelta {
            text: "working".to_owned(),
        },
        ExternalAgentEvent::CommandFinished {
            exit_code: Some(0),
            stdout_tail: "ok".to_owned(),
            stderr_tail: String::new(),
        },
        ExternalAgentEvent::ToolFinished {
            name: "apply_patch".to_owned(),
            status: ToolStatus::Ok,
        },
        ExternalAgentEvent::MessageSent {
            to: agent_id(),
            summary: "handoff".to_owned(),
        },
        ExternalAgentEvent::SessionCompleted,
    ])
}

fn assert_json_round_trip<T>(value: &T)
where
    T: Debug + PartialEq + Serialize + DeserializeOwned,
{
    let encoded = serde_json::to_value(value).expect("serialize");
    let decoded: T = serde_json::from_value(encoded).expect("deserialize");
    assert_eq!(&decoded, value);
}

#[test]
fn external_dto_roundtrips() {
    let request = sample_request();
    assert_json_round_trip(&request);

    let completed = ExternalSessionResult::Completed {
        session: session_ref(),
        output: ExternalAgentOutput {
            summary: "done".to_owned(),
            artifacts: vec![ExternalArtifactRef {
                kind: ExternalArtifactKind::Patch,
                summary: "parser refactor".to_owned(),
                path: Some("src/parser.rs".to_owned()),
                reference: Some("blob://abc".to_owned()),
            }],
            usage: Some(Usage {
                input: 100,
                output: 40,
                ..Usage::default()
            }),
            cost_micros: Some(1_250),
        },
        observations: sample_observations(),
    };
    assert_json_round_trip(&completed);

    let paused = ExternalSessionResult::PausedForInteraction {
        session: session_ref(),
        action_id: "act-1".to_owned(),
        request: Interaction::question(step_id(), "Delete build/ ?".to_owned()),
        observations: vec![ExternalObservedEvent::new(
            5,
            ExternalAgentEvent::PermissionRequested {
                action_id: "act-1".to_owned(),
                summary: "remove build/".to_owned(),
            },
        )],
    };
    assert_json_round_trip(&paused);

    let failed = ExternalSessionResult::Failed {
        session: Some(session_ref()),
        error: ExternalAgentError::ShutdownFailed {
            session: session_ref(),
            detail: "child process would not exit".to_owned(),
        },
        observations: Vec::new(),
    };
    assert_json_round_trip(&failed);
}

#[test]
fn external_session_result_variants_serialize_snake_case() {
    let completed = ExternalSessionResult::Completed {
        session: session_ref(),
        output: ExternalAgentOutput {
            summary: "done".to_owned(),
            artifacts: Vec::new(),
            usage: None,
            cost_micros: None,
        },
        observations: Vec::new(),
    };
    let encoded = serde_json::to_value(&completed).expect("serialize");
    assert!(encoded.get("completed").is_some());

    let launch = ExternalAgentError::Launch {
        runtime: ExternalRuntimeKind::Codex,
        detail: "binary missing".to_owned(),
    };
    let encoded = serde_json::to_value(&launch).expect("serialize error");
    assert!(encoded.get("launch").is_some());
}

#[test]
fn external_error_roundtrips() {
    // Every classified error variant survives a JSON round-trip, including the
    // capability-gated one added for managed runtimes.
    let errors = vec![
        ExternalAgentError::Launch {
            runtime: ExternalRuntimeKind::ClaudeCode,
            detail: "binary missing".to_owned(),
        },
        ExternalAgentError::SessionLost {
            session: Some(session_ref()),
            detail: "process crashed".to_owned(),
        },
        ExternalAgentError::Protocol {
            detail: "unexpected frame".to_owned(),
        },
        ExternalAgentError::LimitExceeded {
            limit: "max_turns=16".to_owned(),
        },
        ExternalAgentError::ResumeUnavailable {
            session: session_ref(),
            detail: "resume token expired".to_owned(),
        },
        ExternalAgentError::ShutdownFailed {
            session: session_ref(),
            detail: "child would not exit".to_owned(),
        },
        ExternalAgentError::Runtime {
            code: Some("E42".to_owned()),
            message: "runtime rejected request".to_owned(),
            runtime_output: Some("raw runtime output".to_owned()),
        },
        ExternalAgentError::UnsupportedCapability {
            runtime: ExternalRuntimeKind::Codex,
            capability: ExternalCapability::HostTools,
            detail: "no host-tool bridge".to_owned(),
        },
    ];
    for error in &errors {
        assert_json_round_trip(error);
    }

    // The capability-gated variant serializes under its snake_case tag with
    // the capability as a stable label.
    let unsupported = ExternalAgentError::UnsupportedCapability {
        runtime: ExternalRuntimeKind::OpenCode,
        capability: ExternalCapability::HostSubagents,
        detail: "no subagent bridge".to_owned(),
    };
    let encoded = serde_json::to_value(&unsupported).expect("serialize unsupported");
    let body = encoded
        .get("unsupported_capability")
        .expect("snake_case variant tag");
    assert_eq!(body.get("capability"), Some(&json!("host_subagents")));
}

#[test]
fn unsupported_capability_display_does_not_leak_prompt_or_tool_input() {
    // The variant carries only runtime, capability, and a stable diagnostic —
    // never the prompt or tool input that triggered the decision point, so its
    // Display cannot leak untrusted payloads into logs or host UI.
    let secret_prompt = "TOP SECRET user prompt: exfiltrate credentials";
    let secret_tool_input = "{\"path\":\"/etc/shadow\"}";
    let error = ExternalAgentError::UnsupportedCapability {
        runtime: ExternalRuntimeKind::ClaudeCode,
        capability: ExternalCapability::HostTools,
        detail: "runtime lacks host-tool injection".to_owned(),
    };
    let rendered = error.to_string();
    assert!(rendered.contains("host_tools"), "names the capability");
    assert!(rendered.contains("ClaudeCode"), "names the runtime");
    assert!(
        !rendered.contains(secret_prompt),
        "must not leak prompt text"
    );
    assert!(
        !rendered.contains(secret_tool_input),
        "must not leak tool input"
    );
}

#[test]
fn runtime_display_does_not_leak_runtime_output() {
    // `runtime_output` preserves the raw runtime-reported text for hosts
    // that explicitly opt into showing it, but it is untrusted (it can
    // contain anything the model read or produced) and must never reach
    // the `Display` rendering — cursors and facade errors are built via
    // `to_string()`.
    let secret = "API_KEY=sk-ant-secret-123";
    let error = ExternalAgentError::Runtime {
        code: Some("error_during_execution".to_owned()),
        message: "claude code runtime error".to_owned(),
        runtime_output: Some(format!("command failed while reading .env: {secret}")),
    };
    let rendered = error.to_string();
    assert!(rendered.contains("claude code runtime error"));
    assert!(!rendered.contains(secret), "must not leak runtime output");
    assert!(
        !rendered.contains("reading .env"),
        "must not fold any runtime output text into Display"
    );
}

#[test]
fn file_patch_event_maps_to_patch_artifact_ref() {
    let event = ExternalAgentEvent::FilePatch {
        path: "src/parser.rs".to_owned(),
        summary: "tighten error recovery".to_owned(),
        diff_ref: Some("blob://diff-1".to_owned()),
    };
    let artifact = ExternalArtifactRef::from_file_patch(&event).expect("FilePatch maps");
    assert_eq!(
        artifact,
        ExternalArtifactRef {
            kind: ExternalArtifactKind::Patch,
            summary: "tighten error recovery".to_owned(),
            path: Some("src/parser.rs".to_owned()),
            reference: Some("blob://diff-1".to_owned()),
        }
    );

    // A FilePatch without a stored diff still maps, leaving `reference` empty.
    let no_ref = ExternalAgentEvent::FilePatch {
        path: "README.md".to_owned(),
        summary: "note".to_owned(),
        diff_ref: None,
    };
    let artifact = ExternalArtifactRef::from_file_patch(&no_ref).expect("FilePatch maps");
    assert_eq!(artifact.reference, None);
    assert_eq!(artifact.path.as_deref(), Some("README.md"));

    // Non-FilePatch events do not map.
    assert!(ExternalArtifactRef::from_file_patch(&ExternalAgentEvent::SessionCompleted).is_none());
}

#[test]
fn collect_file_patch_artifacts_keeps_only_patches_in_order() {
    let events = vec![
        ExternalAgentEvent::SessionStarted { session_id: None },
        ExternalAgentEvent::FilePatch {
            path: "a.rs".to_owned(),
            summary: "first".to_owned(),
            diff_ref: Some("blob://a".to_owned()),
        },
        ExternalAgentEvent::TextDelta {
            text: "chatter".to_owned(),
        },
        ExternalAgentEvent::FilePatch {
            path: "b.rs".to_owned(),
            summary: "second".to_owned(),
            diff_ref: None,
        },
        ExternalAgentEvent::SessionCompleted,
    ];
    let artifacts = collect_file_patch_artifacts(&events);
    assert_eq!(
        artifacts,
        vec![
            ExternalArtifactRef {
                kind: ExternalArtifactKind::Patch,
                summary: "first".to_owned(),
                path: Some("a.rs".to_owned()),
                reference: Some("blob://a".to_owned()),
            },
            ExternalArtifactRef {
                kind: ExternalArtifactKind::Patch,
                summary: "second".to_owned(),
                path: Some("b.rs".to_owned()),
                reference: None,
            },
        ]
    );

    assert!(collect_file_patch_artifacts(&[]).is_empty());
}

#[test]
fn external_observed_event_roundtrips() {
    // A sequenced observation preserves its seq and inner event across a
    // JSON round-trip, and a buffered list keeps per-event seqs distinct and
    // ordered.
    let observed = ExternalObservedEvent::new(
        7,
        ExternalAgentEvent::TextDelta {
            text: "chunk".to_owned(),
        },
    );
    assert_json_round_trip(&observed);
    assert_eq!(observed.seq, 7);

    let buffer = sample_observations();
    assert_json_round_trip(&buffer);
    let seqs: Vec<u64> = buffer.iter().map(|observed| observed.seq).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn collect_file_patch_artifacts_from_observed_ignores_seqs_and_non_patches() {
    // The sequenced collector keeps only FilePatch observations, in order,
    // regardless of the seq labels each carries.
    let observations = vec![
        ExternalObservedEvent::new(10, ExternalAgentEvent::SessionStarted { session_id: None }),
        ExternalObservedEvent::new(
            11,
            ExternalAgentEvent::FilePatch {
                path: "a.rs".to_owned(),
                summary: "first".to_owned(),
                diff_ref: Some("blob://a".to_owned()),
            },
        ),
        ExternalObservedEvent::new(
            12,
            ExternalAgentEvent::TextDelta {
                text: "chatter".to_owned(),
            },
        ),
        ExternalObservedEvent::new(
            13,
            ExternalAgentEvent::FilePatch {
                path: "b.rs".to_owned(),
                summary: "second".to_owned(),
                diff_ref: None,
            },
        ),
    ];
    let artifacts = collect_file_patch_artifacts_from_observed(&observations);
    assert_eq!(
        artifacts,
        vec![
            ExternalArtifactRef {
                kind: ExternalArtifactKind::Patch,
                summary: "first".to_owned(),
                path: Some("a.rs".to_owned()),
                reference: Some("blob://a".to_owned()),
            },
            ExternalArtifactRef {
                kind: ExternalArtifactKind::Patch,
                summary: "second".to_owned(),
                path: Some("b.rs".to_owned()),
                reference: None,
            },
        ]
    );

    assert!(collect_file_patch_artifacts_from_observed(&[]).is_empty());
}

fn sample_tool_call() -> ExternalToolCall {
    ExternalToolCall {
        provider_call_id: "call_provider_1".to_owned(),
        name: "apply_patch".to_owned(),
        input: json!({ "path": "src/parser.rs" }),
        raw: Some(json!({ "provider_only": true })),
    }
}

#[test]
fn external_tool_dto_roundtrips() {
    // The tool decision point and its response both survive a JSON round-trip.
    let paused = ExternalSessionResult::PausedForToolCalls {
        session: session_ref(),
        batch_id: ExternalToolBatchId::new("batch-7"),
        calls: vec![
            sample_tool_call(),
            ExternalToolCall {
                provider_call_id: "call_provider_2".to_owned(),
                name: "run_tests".to_owned(),
                input: json!({}),
                raw: None,
            },
        ],
        observations: vec![ExternalObservedEvent::new(
            9,
            ExternalAgentEvent::ToolStarted {
                name: "apply_patch".to_owned(),
            },
        )],
    };
    assert_json_round_trip(&paused);

    let respond = ExternalSessionInput::RespondToolResults {
        batch_id: ExternalToolBatchId::new("batch-7"),
        results: vec![
            ExternalToolResult {
                provider_call_id: "call_provider_1".to_owned(),
                status: ToolStatus::Ok,
                content: vec![ContentBlock::Text {
                    text: "patched".to_owned(),
                    extra: Map::new(),
                }],
                error: None,
                raw: Some(json!({ "provider_only": 1 })),
            },
            ExternalToolResult {
                provider_call_id: "call_provider_2".to_owned(),
                status: ToolStatus::Error,
                content: Vec::new(),
                error: Some("tests failed".to_owned()),
                raw: None,
            },
        ],
    };
    assert_json_round_trip(&respond);

    // The batch id is serde-transparent: it encodes as the bare string.
    let encoded = serde_json::to_value(ExternalToolBatchId::new("batch-7")).expect("serialize");
    assert_eq!(encoded, json!("batch-7"));
}

#[test]
fn external_tool_input_and_result_variants_serialize_snake_case() {
    let respond = ExternalSessionInput::RespondToolResults {
        batch_id: ExternalToolBatchId::new("batch-1"),
        results: Vec::new(),
    };
    let encoded = serde_json::to_value(&respond).expect("serialize input");
    assert!(encoded.get("respond_tool_results").is_some());

    let paused = ExternalSessionResult::PausedForToolCalls {
        session: session_ref(),
        batch_id: ExternalToolBatchId::new("batch-1"),
        calls: Vec::new(),
        observations: Vec::new(),
    };
    let encoded = serde_json::to_value(&paused).expect("serialize result");
    assert!(encoded.get("paused_for_tool_calls").is_some());
}

#[test]
fn external_tool_call_maps_to_provider_neutral_tool_call() {
    // The provider correlation id, tool name, and input are preserved so a
    // host response can answer the runtime's own call id; the `raw` escape
    // hatch is dropped from the stable tool-execution shape.
    let call = sample_tool_call();
    let bridged = call.to_tool_call();
    assert_eq!(
        bridged,
        ToolCall {
            id: "call_provider_1".to_owned(),
            name: "apply_patch".to_owned(),
            input: json!({ "path": "src/parser.rs" }),
            extra: Map::new(),
        }
    );
}

#[test]
fn tool_response_maps_to_external_result_preserving_status_and_content() {
    for status in [
        ToolStatus::Ok,
        ToolStatus::Error,
        ToolStatus::Denied,
        ToolStatus::Cancelled,
    ] {
        let response = ToolResponse {
            tool_call_id: "call_provider_1".to_owned(),
            content: vec![ContentBlock::Text {
                text: "tool output".to_owned(),
                extra: Map::new(),
            }],
            status,
            extra: Map::from_iter([("provider_trace".to_owned(), json!("trace-1"))]),
        };
        let external = ExternalToolResult::from_tool_response(&response);
        assert_eq!(external.provider_call_id, "call_provider_1");
        assert_eq!(external.status, status);
        assert_eq!(external.content, response.content);
        // A response that ran carries its detail in content; the separate
        // orchestration-error slot stays empty.
        assert_eq!(external.error, None);
        assert_eq!(external.raw, None);
    }
}

#[test]
fn tool_runtime_error_maps_to_external_result_without_losing_error_text() {
    let error = ToolRuntimeError::UnknownTool {
        name: "apply_patch".to_owned(),
    };
    let external = ExternalToolResult::from_tool_runtime_error("call_provider_1", &error);
    let detail = error.to_string();

    assert_eq!(external.provider_call_id, "call_provider_1");
    assert_eq!(external.status, ToolStatus::Error);
    assert_eq!(external.error.as_deref(), Some(detail.as_str()));
    // The same stable text is echoed as tool output so the runtime sees it.
    assert_eq!(
        external.content,
        vec![ContentBlock::Text {
            text: detail,
            extra: Map::new(),
        }]
    );
    assert_eq!(external.raw, None);

    // The mapping round-trips like any other DTO.
    assert_json_round_trip(&external);
}

fn sample_subagent_request() -> ExternalSubagentRequest {
    ExternalSubagentRequest {
        request_id: ExternalSubagentRequestId::new("spawn-3"),
        spec_ref: AgentSpecRef(agent_id()),
        brief: Interaction::question(step_id(), "Investigate the flaky test.".to_owned()),
        result_schema: Some(json!({ "type": "object" })),
        raw: Some(json!({ "provider_only": true })),
    }
}

#[test]
fn external_subagent_dto_roundtrips() {
    // The subagent decision point and its response both survive a JSON
    // round-trip, including the optional escape hatches.
    let paused = ExternalSessionResult::PausedForSubagent {
        session: session_ref(),
        request: sample_subagent_request(),
        observations: vec![ExternalObservedEvent::new(
            11,
            ExternalAgentEvent::TaskUpdated {
                task_id: "spawn-3".to_owned(),
                status: "running".to_owned(),
            },
        )],
    };
    assert_json_round_trip(&paused);

    let respond = ExternalSessionInput::RespondSubagent {
        request_id: ExternalSubagentRequestId::new("spawn-3"),
        output: ExternalSubagentOutput {
            summary: "root cause found".to_owned(),
            raw: Some(json!({ "provider_only": 1 })),
        },
    };
    assert_json_round_trip(&respond);

    // A request with no optional provider fields also round-trips.
    let minimal = ExternalSubagentRequest {
        request_id: ExternalSubagentRequestId::new("spawn-4"),
        spec_ref: AgentSpecRef(agent_id()),
        brief: Interaction::question(step_id(), "Summarise the diff.".to_owned()),
        result_schema: None,
        raw: None,
    };
    assert_json_round_trip(&minimal);

    // The request id is serde-transparent: it encodes as the bare string.
    let encoded =
        serde_json::to_value(ExternalSubagentRequestId::new("spawn-3")).expect("serialize");
    assert_eq!(encoded, json!("spawn-3"));
}

#[test]
fn external_subagent_input_and_result_variants_serialize_snake_case() {
    let respond = ExternalSessionInput::RespondSubagent {
        request_id: ExternalSubagentRequestId::new("spawn-1"),
        output: ExternalSubagentOutput {
            summary: "done".to_owned(),
            raw: None,
        },
    };
    let encoded = serde_json::to_value(&respond).expect("serialize input");
    assert!(encoded.get("respond_subagent").is_some());

    let paused = ExternalSessionResult::PausedForSubagent {
        session: session_ref(),
        request: sample_subagent_request(),
        observations: Vec::new(),
    };
    let encoded = serde_json::to_value(&paused).expect("serialize result");
    assert!(encoded.get("paused_for_subagent").is_some());
}

#[test]
fn subagent_output_maps_from_host_result_preserving_summary() {
    // The runtime-only host result bridges into the persistable DTO, keeping
    // the summary while leaving the provider escape hatch empty.
    let output = ExternalSubagentOutput::from(SubagentOutput {
        summary: "child complete".to_owned(),
    });
    assert_eq!(output.summary, "child complete");
    assert_eq!(output.raw, None);
    assert_json_round_trip(&output);
}
