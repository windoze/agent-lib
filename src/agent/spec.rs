//! Static Agent configuration data.

use crate::{
    agent::id::{AgentId, ToolSetId},
    model::{extras::ProviderExtras, tool::Tool},
};
use serde::{Deserialize, Serialize};
use std::{
    num::NonZeroU32,
    path::{Path, PathBuf},
};

/// Data-only recipe for constructing or restoring an Agent runtime.
///
/// `AgentSpec` is a template: it records stable identity, worktree, model,
/// initial system prompt, initial tool declarations, and loop policy. It does
/// not hold a live [`crate::conversation::Conversation`],
/// [`crate::client::LlmClient`], tool registry, stream, task handle, clock, or
/// cancellation handle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentSpec {
    id: AgentId,
    worktree: WorktreeRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_prompt: Option<String>,
    initial_tools: ToolSetRef,
    model: ModelRef,
    loop_policy: LoopPolicy,
}

impl AgentSpec {
    /// Creates a static Agent recipe from caller-supplied data.
    #[must_use]
    pub const fn new(
        id: AgentId,
        worktree: WorktreeRef,
        system_prompt: Option<String>,
        initial_tools: ToolSetRef,
        model: ModelRef,
        loop_policy: LoopPolicy,
    ) -> Self {
        Self {
            id,
            worktree,
            system_prompt,
            initial_tools,
            model,
            loop_policy,
        }
    }

    /// Returns the externally supplied Agent identity.
    #[must_use]
    pub const fn id(&self) -> AgentId {
        self.id
    }

    /// Returns the worktree boundary configured for this Agent.
    #[must_use]
    pub const fn worktree(&self) -> &WorktreeRef {
        &self.worktree
    }

    /// Returns the initial system prompt, if one was configured.
    #[must_use]
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    /// Returns the initial tool-set declaration reference.
    #[must_use]
    pub const fn initial_tools(&self) -> &ToolSetRef {
        &self.initial_tools
    }

    /// Returns the default model request settings.
    #[must_use]
    pub const fn model(&self) -> &ModelRef {
        &self.model
    }

    /// Returns the loop policy configured for future Agent execution.
    #[must_use]
    pub const fn loop_policy(&self) -> &LoopPolicy {
        &self.loop_policy
    }
}

/// Filesystem boundary assigned to an Agent.
///
/// The value is caller supplied and is not canonicalized or checked by this
/// data model; runtimes can validate it against their own sandbox policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorktreeRef {
    path: PathBuf,
}

impl WorktreeRef {
    /// Creates a worktree reference from caller-supplied path data.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the stored worktree path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Static declaration of the tools initially available to an Agent.
///
/// The id is stable bookkeeping for a registry or persisted row, while
/// [`Tool`] values reuse the provider-neutral declaration shape already used by
/// [`crate::client::ChatRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSetRef {
    id: ToolSetId,
    #[serde(default)]
    tools: Vec<Tool>,
}

impl ToolSetRef {
    /// Creates a static tool-set reference and declaration list.
    #[must_use]
    pub fn new(id: ToolSetId, tools: Vec<Tool>) -> Self {
        Self { id, tools }
    }

    /// Returns the stable tool-set identity.
    #[must_use]
    pub const fn id(&self) -> ToolSetId {
        self.id
    }

    /// Returns the provider-neutral tool declarations in this set.
    #[must_use]
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }
}

/// Default model request settings for an Agent.
///
/// This type stores only data that can later be copied into a
/// [`crate::client::ChatRequest`]. It does not hold a client object, endpoint
/// transport, or authentication handle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelRef {
    model: String,
    max_tokens: NonZeroU32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_extras: Option<ProviderExtras>,
}

impl ModelRef {
    /// Creates model request settings from caller-supplied data.
    #[must_use]
    pub fn new(
        model: impl Into<String>,
        max_tokens: NonZeroU32,
        temperature: Option<f32>,
        provider_extras: Option<ProviderExtras>,
    ) -> Self {
        Self {
            model: model.into(),
            max_tokens,
            temperature,
            provider_extras,
        }
    }

    /// Returns the model or deployment identifier.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the configured maximum output token count.
    #[must_use]
    pub const fn max_tokens(&self) -> NonZeroU32 {
        self.max_tokens
    }

    /// Returns the configured sampling temperature, if one was set.
    #[must_use]
    pub const fn temperature(&self) -> Option<f32> {
        self.temperature
    }

    /// Returns provider-specific request extras, if configured.
    #[must_use]
    pub const fn provider_extras(&self) -> Option<&ProviderExtras> {
        self.provider_extras.as_ref()
    }

    /// Returns a copy of this model with provider-specific extras replaced.
    #[must_use]
    pub(crate) fn with_provider_extras(mut self, provider_extras: ProviderExtras) -> Self {
        self.provider_extras = Some(provider_extras);
        self
    }
}

/// Static policy knobs used by a future Agent loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopPolicy {
    max_steps: NonZeroU32,
    max_parallel_tools: NonZeroU32,
    tool_failure_policy: ToolFailurePolicy,
}

impl LoopPolicy {
    /// Creates loop policy data from explicit non-zero limits.
    #[must_use]
    pub const fn new(
        max_steps: NonZeroU32,
        max_parallel_tools: NonZeroU32,
        tool_failure_policy: ToolFailurePolicy,
    ) -> Self {
        Self {
            max_steps,
            max_parallel_tools,
            tool_failure_policy,
        }
    }

    /// Returns the maximum number of loop steps allowed for one feed segment.
    #[must_use]
    pub const fn max_steps(&self) -> NonZeroU32 {
        self.max_steps
    }

    /// Returns the maximum number of tool calls a loop may execute at once.
    #[must_use]
    pub const fn max_parallel_tools(&self) -> NonZeroU32 {
        self.max_parallel_tools
    }

    /// Returns the configured tool failure behavior.
    #[must_use]
    pub const fn tool_failure_policy(&self) -> ToolFailurePolicy {
        self.tool_failure_policy
    }
}

impl Default for LoopPolicy {
    fn default() -> Self {
        Self {
            max_steps: NonZeroU32::new(32).expect("default max_steps is non-zero"),
            max_parallel_tools: NonZeroU32::new(1).expect("default max_parallel_tools is non-zero"),
            tool_failure_policy: ToolFailurePolicy::ReturnErrorToModel,
        }
    }
}

/// Policy for converting tool execution failures at the Agent boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolFailurePolicy {
    /// Return the tool error to the model as a normal failed tool result.
    ReturnErrorToModel,
    /// Stop the current Agent run segment when a tool fails.
    StopRun,
}

#[cfg(test)]
mod tests {
    use super::{AgentSpec, LoopPolicy, ModelRef, ToolFailurePolicy, ToolSetRef, WorktreeRef};
    use crate::{
        agent::id::{AgentId, ToolSetId},
        model::{
            extras::{ProviderExtras, ProviderId},
            tool::Tool,
        },
    };
    use serde_json::{Map, json};
    use std::num::NonZeroU32;

    fn nz(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("test value is non-zero")
    }

    fn tool_set_id() -> ToolSetId {
        "018f0d9c-7b6a-7c12-8f31-1234567890b1"
            .parse()
            .expect("tool set id")
    }

    fn agent_id() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890b2"
            .parse()
            .expect("agent id")
    }

    fn weather_tool() -> Tool {
        Tool {
            name: "get_weather".to_owned(),
            description: "Get current weather for a city.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" }
                },
                "required": ["city"]
            }),
        }
    }

    fn model_ref() -> ModelRef {
        ModelRef::new(
            "gpt-5.5",
            nz(1_024),
            Some(0.2),
            Some(ProviderExtras {
                provider: ProviderId::OpenAiResp,
                fields: Map::from_iter([("reasoning_effort".to_owned(), json!("low"))]),
            }),
        )
    }

    #[test]
    fn agent_spec_serde_preserves_external_static_values() {
        let spec = AgentSpec::new(
            agent_id(),
            WorktreeRef::new("/repo/agent-lib"),
            Some("Answer concisely.".to_owned()),
            ToolSetRef::new(tool_set_id(), vec![weather_tool()]),
            model_ref(),
            LoopPolicy::new(nz(16), nz(4), ToolFailurePolicy::StopRun),
        );

        let encoded = serde_json::to_value(&spec).expect("serialize agent spec");
        assert_eq!(encoded["id"], json!("018f0d9c-7b6a-7c12-8f31-1234567890b2"));
        assert_eq!(encoded["worktree"], json!("/repo/agent-lib"));
        assert_eq!(encoded["system_prompt"], json!("Answer concisely."));
        assert_eq!(
            encoded["initial_tools"]["id"],
            json!("018f0d9c-7b6a-7c12-8f31-1234567890b1")
        );
        assert_eq!(
            encoded["initial_tools"]["tools"][0]["name"],
            json!("get_weather")
        );
        assert_eq!(encoded["model"]["model"], json!("gpt-5.5"));
        assert_eq!(encoded["model"]["max_tokens"], json!(1_024));
        assert_eq!(
            encoded["model"]["provider_extras"]["provider"],
            json!("open_ai_resp")
        );
        assert_eq!(encoded["loop_policy"]["max_steps"], json!(16));
        assert_eq!(encoded["loop_policy"]["max_parallel_tools"], json!(4));
        assert_eq!(
            encoded["loop_policy"]["tool_failure_policy"],
            json!("stop_run")
        );

        let decoded: AgentSpec = serde_json::from_value(encoded).expect("deserialize agent spec");
        assert_eq!(decoded, spec);
        assert_eq!(decoded.id(), agent_id());
        assert_eq!(
            decoded.worktree().path(),
            std::path::Path::new("/repo/agent-lib")
        );
        assert_eq!(decoded.system_prompt(), Some("Answer concisely."));
        assert_eq!(decoded.initial_tools().id(), tool_set_id());
        assert_eq!(decoded.initial_tools().tools()[0].name, "get_weather");
        assert_eq!(decoded.model().model(), "gpt-5.5");
        assert_eq!(decoded.model().max_tokens(), nz(1_024));
        assert_eq!(decoded.loop_policy().max_steps(), nz(16));
    }

    #[test]
    fn agent_spec_shape_contains_no_runtime_handles() {
        let spec = AgentSpec::new(
            agent_id(),
            WorktreeRef::new("/repo/agent-lib"),
            None,
            ToolSetRef::new(tool_set_id(), Vec::new()),
            ModelRef::new("gpt-5.5", nz(512), None, None),
            LoopPolicy::default(),
        );

        let encoded = serde_json::to_value(&spec).expect("serialize agent spec");
        let object = encoded.as_object().expect("agent spec object");

        for forbidden in [
            "conversation",
            "client",
            "llm_client",
            "tool_registry",
            "runtime",
            "task_handle",
            "cancel",
            "stream",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "runtime handle key must not be serialized: {forbidden}"
            );
        }
        assert_eq!(object.keys().count(), 5);
        assert_eq!(encoded["loop_policy"]["max_steps"], json!(32));
        assert_eq!(encoded["loop_policy"]["max_parallel_tools"], json!(1));
    }

    #[test]
    fn zero_nonzero_policy_limits_are_rejected_by_deserialization() {
        let encoded = json!({
            "id": "018f0d9c-7b6a-7c12-8f31-1234567890b2",
            "worktree": "/repo/agent-lib",
            "initial_tools": {
                "id": "018f0d9c-7b6a-7c12-8f31-1234567890b1",
                "tools": []
            },
            "model": {
                "model": "gpt-5.5",
                "max_tokens": 0
            },
            "loop_policy": {
                "max_steps": 0,
                "max_parallel_tools": 1,
                "tool_failure_policy": "return_error_to_model"
            }
        });

        let error = serde_json::from_value::<AgentSpec>(encoded)
            .expect_err("non-zero fields must reject zero");

        assert!(!error.to_string().is_empty());
    }
}
