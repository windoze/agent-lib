//! Normalized enum wrappers that preserve provider raw values.

use serde::{Deserialize, Serialize};

/// A provider value mapped into a stable enum while retaining the original wire
/// value for diagnostics and forward compatibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Normalized<T> {
    /// The provider-neutral value used by the rest of the client layer.
    pub value: T,
    /// The raw provider value that produced `value`, when one was available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

impl<T> Normalized<T> {
    /// Builds a normalized value from a known provider mapping.
    pub fn from_mapped(value: T, raw: impl Into<String>) -> Self {
        Self {
            value,
            raw: Some(raw.into()),
        }
    }
}

impl<T: UnknownNormalizedValue> Normalized<T> {
    /// Builds a normalized value for an unknown provider string.
    pub fn unknown(raw: impl Into<String>) -> Self {
        Self {
            value: T::unknown_value(),
            raw: Some(raw.into()),
        }
    }
}

/// Supplies the fallback enum value used by `Normalized::unknown`.
pub trait UnknownNormalizedValue {
    /// Returns the provider-neutral value that represents an unmapped raw value.
    fn unknown_value() -> Self;
}

/// Provider-neutral reasons for a model finishing a response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model stopped because it emitted a tool call.
    ToolUse,
    /// The model completed its assistant turn normally.
    EndTurn,
    /// The model stopped after reaching its token limit.
    MaxTokens,
    /// The model stopped after matching a configured stop sequence.
    StopSequence,
    /// The provider refused to generate the requested content.
    Refusal,
    /// The provider returned a stop reason this crate does not yet model.
    Other,
}

impl StopReason {
    /// Normalizes a raw provider stop-reason string.
    pub fn normalize(raw: impl Into<String>) -> Normalized<Self> {
        let raw = raw.into();
        match raw.as_str() {
            "tool_use" => Normalized::from_mapped(Self::ToolUse, raw),
            "end_turn" => Normalized::from_mapped(Self::EndTurn, raw),
            "max_tokens" => Normalized::from_mapped(Self::MaxTokens, raw),
            "stop_sequence" => Normalized::from_mapped(Self::StopSequence, raw),
            "refusal" => Normalized::from_mapped(Self::Refusal, raw),
            _ => Normalized::unknown(raw),
        }
    }
}

impl UnknownNormalizedValue for StopReason {
    fn unknown_value() -> Self {
        Self::Other
    }
}

#[cfg(test)]
mod tests {
    use super::{Normalized, StopReason};

    #[test]
    fn normalizes_known_stop_reason_and_keeps_raw() {
        let reason = StopReason::normalize("tool_use");

        assert_eq!(reason.value, StopReason::ToolUse);
        assert_eq!(reason.raw.as_deref(), Some("tool_use"));
    }

    #[test]
    fn normalizes_unknown_stop_reason_to_other_and_keeps_raw() {
        let reason = StopReason::normalize("weird");

        assert_eq!(reason.value, StopReason::Other);
        assert_eq!(reason.raw.as_deref(), Some("weird"));
    }

    #[test]
    fn every_normalized_stop_reason_round_trips_through_serde() {
        for (value, raw) in [
            (StopReason::ToolUse, "tool_use"),
            (StopReason::EndTurn, "end_turn"),
            (StopReason::MaxTokens, "max_tokens"),
            (StopReason::StopSequence, "stop_sequence"),
            (StopReason::Refusal, "refusal"),
            (StopReason::Other, "provider_specific"),
        ] {
            let reason = Normalized::from_mapped(value, raw);
            let json = serde_json::to_string(&reason).expect("serialize normalized stop reason");
            let decoded: Normalized<StopReason> =
                serde_json::from_str(&json).expect("deserialize normalized stop reason");

            assert_eq!(decoded, reason);
        }
    }

    #[test]
    fn stop_reason_uses_snake_case_wire_names() {
        let json = serde_json::to_string(&StopReason::StopSequence).expect("serialize stop reason");

        assert_eq!(json, "\"stop_sequence\"");
    }
}
