//! Structured descriptions of provider and model capabilities.

use crate::model::normalized::StopReason;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::LazyLock};

/// A content modality accepted or produced by an LLM endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    /// UTF-8 text content.
    Text,
    /// Encoded image content or an image URL.
    Image,
    /// Encoded audio content or an audio stream.
    Audio,
    /// A file attachment such as a PDF or document.
    File,
}

/// A structured description of the features supported by an LLM endpoint.
///
/// Default table entries describe protocol-level support. Callers can clone a
/// table entry and override public fields with model- or deployment-specific
/// limits without reducing multimodal support to a single boolean flag.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    /// Maximum context window for a specific model, when known.
    pub max_context_tokens: Option<u32>,
    /// Content modalities accepted in requests.
    pub input_modalities: BTreeSet<Modality>,
    /// Content modalities that responses can produce.
    pub output_modalities: BTreeSet<Modality>,
    /// Whether incremental streaming responses are supported.
    pub streaming: bool,
    /// Whether the model can call declared tools.
    pub tool_calling: bool,
    /// Whether one model turn can request more than one tool call.
    pub parallel_tool_calls: bool,
    /// Whether the endpoint supports prompt or input caching.
    pub prompt_caching: bool,
    /// Whether the endpoint exposes reasoning or thinking content.
    pub reasoning: bool,
    /// Whether schema-constrained structured output is supported.
    pub structured_output: bool,
    /// Normalized stop reasons the endpoint can report.
    pub stop_reasons: BTreeSet<StopReason>,
}

/// Protocol-level default capabilities for Anthropic Messages endpoints.
///
/// The context limit remains unknown because it is model-specific. Clone this
/// value before applying model or deployment overrides.
pub static ANTHROPIC_DEFAULT_CAPABILITY: LazyLock<Capability> = LazyLock::new(|| Capability {
    max_context_tokens: None,
    input_modalities: set([Modality::Text, Modality::Image, Modality::File]),
    output_modalities: set([Modality::Text]),
    streaming: true,
    tool_calling: true,
    parallel_tool_calls: true,
    prompt_caching: true,
    reasoning: true,
    structured_output: true,
    stop_reasons: set([
        StopReason::ToolUse,
        StopReason::EndTurn,
        StopReason::MaxTokens,
        StopReason::StopSequence,
        StopReason::Refusal,
    ]),
});

/// Protocol-level default capabilities for OpenAI Responses endpoints.
///
/// The context limit remains unknown because it is model-specific. Clone this
/// value before applying model or deployment overrides.
pub static OPENAI_RESP_DEFAULT_CAPABILITY: LazyLock<Capability> = LazyLock::new(|| Capability {
    max_context_tokens: None,
    input_modalities: set([
        Modality::Text,
        Modality::Image,
        Modality::Audio,
        Modality::File,
    ]),
    output_modalities: set([Modality::Text, Modality::Audio]),
    streaming: true,
    tool_calling: true,
    parallel_tool_calls: true,
    prompt_caching: true,
    reasoning: true,
    structured_output: true,
    stop_reasons: set([
        StopReason::ToolUse,
        StopReason::EndTurn,
        StopReason::MaxTokens,
        StopReason::Refusal,
    ]),
});

/// Protocol-level default capabilities for OpenAI Chat/Completions endpoints
/// (classic `POST /v1/chat/completions`, shared by OpenAI-compatible baselines,
/// DeepSeek, and vLLM).
///
/// The context limit remains unknown because it is model-specific. Clone this
/// value before applying model or deployment overrides.
pub static OPENAI_CHAT_DEFAULT_CAPABILITY: LazyLock<Capability> = LazyLock::new(|| Capability {
    max_context_tokens: None,
    input_modalities: set([Modality::Text, Modality::Image]),
    output_modalities: set([Modality::Text]),
    streaming: true,
    tool_calling: true,
    parallel_tool_calls: true,
    prompt_caching: false,
    reasoning: true,
    structured_output: false,
    stop_reasons: set([
        StopReason::ToolUse,
        StopReason::EndTurn,
        StopReason::MaxTokens,
        StopReason::StopSequence,
        StopReason::Refusal,
    ]),
});

/// Builds an ordered set for deterministic capability serialization.
fn set<T: Ord, const N: usize>(values: [T; N]) -> BTreeSet<T> {
    BTreeSet::from(values)
}

#[cfg(test)]
mod tests {
    use super::{
        ANTHROPIC_DEFAULT_CAPABILITY, Capability, Modality, OPENAI_CHAT_DEFAULT_CAPABILITY,
        OPENAI_RESP_DEFAULT_CAPABILITY,
    };
    use crate::model::normalized::StopReason;
    use std::collections::BTreeSet;

    #[test]
    fn every_modality_round_trips_through_its_snake_case_wire_name() {
        for (modality, wire_name) in [
            (Modality::Text, "text"),
            (Modality::Image, "image"),
            (Modality::Audio, "audio"),
            (Modality::File, "file"),
        ] {
            let encoded = serde_json::to_string(&modality).expect("serialize modality");
            assert_eq!(encoded, format!("\"{wire_name}\""));

            let decoded: Modality = serde_json::from_str(&encoded).expect("deserialize modality");
            assert_eq!(decoded, modality);
        }
    }

    #[test]
    fn capability_round_trips_through_serde() {
        let capability = Capability {
            max_context_tokens: Some(200_000),
            input_modalities: BTreeSet::from([Modality::Text, Modality::Image]),
            output_modalities: BTreeSet::from([Modality::Text]),
            streaming: true,
            tool_calling: true,
            parallel_tool_calls: true,
            prompt_caching: true,
            reasoning: true,
            structured_output: true,
            stop_reasons: BTreeSet::from([
                StopReason::EndTurn,
                StopReason::MaxTokens,
                StopReason::ToolUse,
            ]),
        };

        let encoded = serde_json::to_string(&capability).expect("serialize capability");
        let decoded: Capability = serde_json::from_str(&encoded).expect("deserialize capability");

        assert_eq!(decoded, capability);
    }

    #[test]
    fn anthropic_default_describes_protocol_capabilities() {
        let capability = &*ANTHROPIC_DEFAULT_CAPABILITY;

        assert_eq!(capability.max_context_tokens, None);
        assert_eq!(
            capability.input_modalities,
            BTreeSet::from([Modality::Text, Modality::Image, Modality::File])
        );
        assert_eq!(
            capability.output_modalities,
            BTreeSet::from([Modality::Text])
        );
        assert!(capability.streaming);
        assert!(capability.tool_calling);
        assert!(capability.parallel_tool_calls);
        assert!(capability.prompt_caching);
        assert!(capability.reasoning);
        assert!(capability.structured_output);
        assert!(capability.stop_reasons.contains(&StopReason::StopSequence));
    }

    #[test]
    fn openai_response_default_describes_protocol_capabilities() {
        let capability = &*OPENAI_RESP_DEFAULT_CAPABILITY;

        assert_eq!(capability.max_context_tokens, None);
        assert_eq!(
            capability.input_modalities,
            BTreeSet::from([
                Modality::Text,
                Modality::Image,
                Modality::Audio,
                Modality::File,
            ])
        );
        assert_eq!(
            capability.output_modalities,
            BTreeSet::from([Modality::Text, Modality::Audio])
        );
        assert!(capability.streaming);
        assert!(capability.tool_calling);
        assert!(capability.parallel_tool_calls);
        assert!(capability.prompt_caching);
        assert!(capability.reasoning);
        assert!(capability.structured_output);
        assert!(!capability.stop_reasons.contains(&StopReason::StopSequence));
    }

    #[test]
    fn openai_chat_default_describes_protocol_capabilities() {
        let capability = &*OPENAI_CHAT_DEFAULT_CAPABILITY;

        assert_eq!(capability.max_context_tokens, None);
        assert_eq!(
            capability.input_modalities,
            BTreeSet::from([Modality::Text, Modality::Image])
        );
        assert_eq!(
            capability.output_modalities,
            BTreeSet::from([Modality::Text])
        );
        assert!(capability.streaming);
        assert!(capability.tool_calling);
        assert!(capability.parallel_tool_calls);
        // chat/completions does not declare prompt caching or structured output.
        assert!(!capability.prompt_caching);
        assert!(capability.reasoning);
        assert!(!capability.structured_output);
        // stop_reasons covers every normalized value reachable from chat/completions,
        // including StopSequence (the classic `stop` parameter) and Refusal.
        assert_eq!(
            capability.stop_reasons,
            BTreeSet::from([
                StopReason::ToolUse,
                StopReason::EndTurn,
                StopReason::MaxTokens,
                StopReason::StopSequence,
                StopReason::Refusal,
            ])
        );
    }

    #[test]
    fn cloned_defaults_can_be_overridden_without_mutating_the_table() {
        let mut overridden = (*ANTHROPIC_DEFAULT_CAPABILITY).clone();
        overridden.max_context_tokens = Some(1_000_000);
        overridden.output_modalities.insert(Modality::Audio);

        assert_eq!(overridden.max_context_tokens, Some(1_000_000));
        assert!(overridden.output_modalities.contains(&Modality::Audio));
        assert_eq!(ANTHROPIC_DEFAULT_CAPABILITY.max_context_tokens, None);
        assert!(
            !ANTHROPIC_DEFAULT_CAPABILITY
                .output_modalities
                .contains(&Modality::Audio)
        );
    }
}
