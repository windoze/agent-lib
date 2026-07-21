//! Reconfigure admission/validation and tool-set apply tests for the [`Agent`]
//! facade, split out of `tests.rs`.

use super::*;

#[tokio::test]
async fn reconfigure_set_model_and_overlay_apply_at_next_turn_start() {
    let client = ScriptedClient::new(vec![text_response("updated")]);
    let mut agent = agent_with(
        client.clone(),
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let model = reconfig_model("test-model-v2");

    agent
        .reconfigure(ReconfigRequest::SetModel {
            model: model.clone(),
        })
        .expect("set-model reconfig is accepted while idle");
    agent
        .reconfigure(ReconfigRequest::set_system_prompt_overlay(
            Some("Prefer the updated runtime config.".to_owned()),
            0,
        ))
        .expect("system overlay reconfig is accepted while idle");

    let reply = agent.run("Use the latest config.").await.unwrap();

    assert_eq!(reply.text(), "updated");
    assert_eq!(agent.state().current_model(), &model);
    assert_eq!(agent.state().system_prompt_overlay_version(), 1);
    assert!(agent.state().queued_reconfigs().is_empty());
    let requests = client.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, "test-model-v2");
    assert_eq!(requests[0].max_tokens, 321);
    assert_eq!(requests[0].temperature, Some(0.25));
    assert_eq!(
        requests[0].system.as_deref(),
        Some("You are a concise weather assistant.\n\nPrefer the updated runtime config.")
    );
}

#[test]
fn reconfigure_rejects_active_turns() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let input = AgentInput::user_message(
        agent.ids.turn_id(),
        agent.ids.message_id(),
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hold the turn".to_owned(),
                extra: Map::new(),
            }],
        },
        agent.ids.message_id(),
        agent.ids.step_id(),
    )
    .expect("valid user input");

    let outcome = agent.machine.step(StepInput::External(input));
    assert_eq!(agent.machine.cursor().kind(), LoopCursorKind::StreamingStep);
    assert_eq!(outcome.requirements.len(), 1);

    let error = agent
        .reconfigure(ReconfigRequest::SetModel {
            model: reconfig_model("late-model"),
        })
        .expect_err("active turn reconfigure is rejected");

    assert!(
        matches!(error, FacadeError::InvalidState(message) if message.contains("between runs"))
    );
    assert!(agent.state().queued_reconfigs().is_empty());
}

#[test]
fn reconfigure_rejects_skill_requests_explicitly() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );

    let error = agent
        .reconfigure(ReconfigRequest::ActivateSkill {
            skill_id: reconfig_skill_id(),
        })
        .expect_err("facade skill reconfig is not supported");

    assert!(matches!(error, FacadeError::Config(message) if message.contains("skill activation")));
    assert!(agent.state().queued_reconfigs().is_empty());
}

#[test]
fn reconfigure_accepts_executable_tool_set_declaration_requests_at_admission() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut replace_agent = agent_with_tools(
        client,
        vec![
            counting_weather_tool(Arc::new(AtomicUsize::new(0))),
            counting_calendar_tool(Arc::new(AtomicUsize::new(0))),
        ],
        Approval::auto_allow(),
    );
    let replacement = ToolSetRef::new(reconfig_tool_set_id(1), vec![calendar_tool_decl()]);

    replace_agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement,
        })
        .expect("replace-tool-set reconfig is accepted at admission");
    assert_eq!(replace_agent.state().queued_reconfigs().len(), 1);

    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut patch_agent = agent_with_tools(
        client,
        vec![
            counting_weather_tool(Arc::new(AtomicUsize::new(0))),
            counting_calendar_tool(Arc::new(AtomicUsize::new(0))),
        ],
        Approval::auto_allow(),
    );
    let patch = ToolSetPatch::new(
        patch_agent.state().current_tool_set().id(),
        reconfig_tool_set_id(2),
        vec!["get_weather".to_owned()],
        Vec::new(),
    )
    .expect("valid tool-set patch");

    patch_agent
        .reconfigure(ReconfigRequest::PatchToolSet { patch })
        .expect("patch-tool-set reconfig is accepted at admission");
    assert_eq!(patch_agent.state().queued_reconfigs().len(), 1);
}

#[test]
fn reconfigure_rejects_tool_set_not_backed_by_facade_registry() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let replacement = ToolSetRef::new(reconfig_tool_set_id(1), vec![calendar_tool_decl()]);

    let error = agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement,
        })
        .expect_err("unbacked tool-set reconfig is rejected");

    assert!(
        matches!(error, FacadeError::Agent(AgentError::Tool(ref tool_error)) if tool_error.to_string().contains("not present in the facade registry")),
        "unexpected error: {error:?}"
    );
    assert!(agent.state().queued_reconfigs().is_empty());
}

#[tokio::test]
async fn reconfigure_replace_tool_set_updates_non_streaming_registry() {
    let client = ScriptedClient::new(vec![
        tool_use_response_for("read_calendar", "call-calendar", json!({ "day": "Monday" })),
        text_response("calendar checked"),
        tool_use_response_for("get_weather", "call-weather", json!({ "city": "Shanghai" })),
        text_response("weather was unavailable"),
    ]);
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let calendar_calls = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with_tools(
        client.clone(),
        vec![
            counting_weather_tool(weather_calls.clone()),
            counting_calendar_tool(calendar_calls.clone()),
        ],
        Approval::auto_allow(),
    );
    let replacement = ToolSetRef::new(reconfig_tool_set_id(1), vec![calendar_tool_decl()]);

    agent
        .reconfigure(ReconfigRequest::ReplaceToolSet {
            tool_set: replacement.clone(),
        })
        .expect("replace-tool-set reconfig is accepted");

    let first = agent
        .run_full("Use the current tool set.")
        .await
        .expect("calendar run succeeds");

    assert_eq!(first.reply.text(), "calendar checked");
    assert_eq!(agent.state().current_tool_set(), &replacement);
    assert!(agent.state().queued_reconfigs().is_empty());
    assert_eq!(calendar_calls.load(Ordering::SeqCst), 1);
    assert_eq!(weather_calls.load(Ordering::SeqCst), 0);

    let second = agent
        .run_full("Try the removed weather tool.")
        .await
        .expect("removed tool error is returned to the model");

    assert_eq!(second.reply.text(), "weather was unavailable");
    assert_eq!(calendar_calls.load(Ordering::SeqCst), 1);
    assert_eq!(weather_calls.load(Ordering::SeqCst), 0);

    let requests = client.requests();
    assert_eq!(tool_names(&requests[0].tools), vec!["read_calendar"]);
    assert_eq!(tool_names(&requests[2].tools), vec!["read_calendar"]);
    assert!(
        messages_contain_tool_error(&requests[3].messages, "unknown tool `get_weather`"),
        "final request should include the removed-tool error result: {:?}",
        requests[3].messages
    );
}

#[tokio::test]
async fn reconfigure_patch_tool_set_updates_non_streaming_registry() {
    let client = ScriptedClient::new(vec![
        tool_use_response_for(
            "read_calendar",
            "call-calendar",
            json!({ "day": "Tuesday" }),
        ),
        text_response("patched calendar checked"),
    ]);
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let calendar_calls = Arc::new(AtomicUsize::new(0));
    let mut agent = agent_with_tools(
        client.clone(),
        vec![
            counting_weather_tool(weather_calls.clone()),
            counting_calendar_tool(calendar_calls.clone()),
        ],
        Approval::auto_allow(),
    );
    let patch = ToolSetPatch::new(
        agent.state().current_tool_set().id(),
        reconfig_tool_set_id(2),
        vec!["get_weather".to_owned()],
        Vec::new(),
    )
    .expect("valid tool-set patch");

    agent
        .reconfigure(ReconfigRequest::PatchToolSet { patch })
        .expect("patch-tool-set reconfig is accepted");

    let output = agent
        .run_full("Use the patched tool set.")
        .await
        .expect("patched calendar run succeeds");

    assert_eq!(output.reply.text(), "patched calendar checked");
    assert_eq!(
        tool_names(agent.state().current_tool_set().tools()),
        vec!["read_calendar"]
    );
    assert_eq!(calendar_calls.load(Ordering::SeqCst), 1);
    assert_eq!(weather_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        tool_names(&client.requests()[0].tools),
        vec!["read_calendar"]
    );
}
#[test]
fn reconfigure_rejects_blank_set_model_at_admission() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );

    let error = agent
        .reconfigure(ReconfigRequest::SetModel {
            model: reconfig_model("  "),
        })
        .expect_err("blank set-model reconfig is rejected");

    assert!(
        matches!(error, FacadeError::Config(ref message) if message.contains("blank `model`")),
        "unexpected error: {error:?}"
    );
    assert!(agent.state().queued_reconfigs().is_empty());
}

#[test]
fn reconfigure_rejects_non_finite_set_model_temperature_at_admission() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut agent = agent_with(
        client,
        counting_weather_tool(Arc::new(AtomicUsize::new(0))),
        Approval::auto_allow(),
    );
    let model = ModelRef::new(
        "test-model-v2",
        NonZeroU32::new(321).expect("non-zero max tokens"),
        Some(f32::NAN),
        None,
    );

    let error = agent
        .reconfigure(ReconfigRequest::SetModel { model })
        .expect_err("non-finite set-model temperature is rejected");

    assert!(
        matches!(error, FacadeError::Config(ref message) if message.contains("non-finite `temperature`")),
        "unexpected error: {error:?}"
    );
    assert!(agent.state().queued_reconfigs().is_empty());
}

#[test]
fn reconfigure_set_model_provider_extras_must_follow_the_current_provider() {
    let client = ScriptedClient::new(vec![text_response("unused")]);
    let mut agent = AgentBuilder::default()
        .client(client)
        .model("test-model")
        .provider_extras(provider_extras(ProviderId::Anthropic))
        .tool(counting_weather_tool(Arc::new(AtomicUsize::new(0))))
        .approval(Approval::auto_allow())
        .build()
        .expect("build agent");
    let mismatched = ModelRef::new(
        "test-model-v2",
        NonZeroU32::new(321).expect("non-zero max tokens"),
        None,
        Some(provider_extras(ProviderId::OpenAiResp)),
    );

    let error = agent
        .reconfigure(ReconfigRequest::SetModel { model: mismatched })
        .expect_err("provider extras targeting another provider are rejected");

    assert!(
        matches!(error, FacadeError::Config(ref message) if message.contains("provider_extras")),
        "unexpected error: {error:?}"
    );
    assert!(agent.state().queued_reconfigs().is_empty());

    let matched = ModelRef::new(
        "test-model-v2",
        NonZeroU32::new(321).expect("non-zero max tokens"),
        None,
        Some(provider_extras(ProviderId::Anthropic)),
    );
    agent
        .reconfigure(ReconfigRequest::SetModel { model: matched })
        .expect("provider extras targeting the current provider are accepted");
    assert_eq!(agent.state().queued_reconfigs().len(), 1);
}

/// Compile-level proof that a facade-only consumer can construct every
/// supported reconfig request from facade re-exports alone, without importing
/// `agent::` internal modules (M2-4).
mod facade_surface {
    use std::num::NonZeroU32;

    use crate::facade::{
        LoopPolicy, ModelRef, ReconfigRequest, ToolDecl, ToolFailurePolicy, ToolSetId,
        ToolSetPatch, ToolSetRef,
    };

    fn tool_set_id(literal: &str) -> ToolSetId {
        ToolSetId::parse_str(literal).expect("tool set id")
    }

    fn weather_decl() -> ToolDecl {
        ToolDecl {
            name: "get_weather".to_owned(),
            description: "Look up the current weather for a city.".to_owned(),
            input_schema: serde_json::json!({ "type": "object" }),
        }
    }

    #[test]
    fn facade_paths_construct_every_supported_reconfig_request() {
        let requests = [
            ReconfigRequest::SetModel {
                model: ModelRef::new(
                    "test-model",
                    NonZeroU32::new(256).expect("non-zero max tokens"),
                    Some(0.5),
                    None,
                ),
            },
            ReconfigRequest::set_system_prompt_overlay(Some("overlay".to_owned()), 0),
            ReconfigRequest::ReplaceToolSet {
                tool_set: ToolSetRef::new(
                    tool_set_id("018f0d9c-7b6a-7c12-8f31-1234567890e1"),
                    vec![weather_decl()],
                ),
            },
            ReconfigRequest::PatchToolSet {
                patch: ToolSetPatch::new(
                    tool_set_id("018f0d9c-7b6a-7c12-8f31-1234567890e1"),
                    tool_set_id("018f0d9c-7b6a-7c12-8f31-1234567890e2"),
                    Vec::new(),
                    vec![weather_decl()],
                )
                .expect("valid tool-set patch"),
            },
            ReconfigRequest::SetLoopPolicy {
                loop_policy: LoopPolicy::new(
                    NonZeroU32::new(4).expect("non-zero step budget"),
                    NonZeroU32::new(1).expect("non-zero parallelism"),
                    ToolFailurePolicy::ReturnErrorToModel,
                ),
            },
        ];
        assert_eq!(requests.len(), 5);
    }
}
