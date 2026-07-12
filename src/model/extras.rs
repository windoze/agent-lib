//! Provider-specific escape hatches for request and response metadata.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// A wire protocol/provider family supported by the client layer.
///
/// Provider identifiers bind request extras to the adapter that understands
/// them. New adapters may extend this enum without changing the extras model.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    /// Anthropic Messages wire protocol.
    Anthropic,
    /// OpenAI Responses wire protocol.
    OpenAiResp,
}

/// Provider-owned request fields that the normalized request model omits.
///
/// Adapters merge these fields only during their final request serialization
/// step. The explicit `provider` binding prevents one provider's dialect from
/// leaking into another provider's request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderExtras {
    /// The only provider allowed to consume these fields.
    pub provider: ProviderId,
    /// Provider-specific request fields keyed by their wire names.
    #[serde(default)]
    pub fields: Map<String, Value>,
}

impl ProviderExtras {
    /// Merges provider-specific fields into a serialized request object.
    ///
    /// A provider mismatch leaves `body` unchanged and returns an observable
    /// [`ProviderExtrasMergeOutcome::IgnoredProviderMismatch`]. When providers
    /// match, extra fields are inserted last and therefore replace body fields
    /// with the same key.
    pub fn merge_into(
        &self,
        body: &mut Value,
        target: ProviderId,
    ) -> Result<ProviderExtrasMergeOutcome, ProviderExtrasMergeError> {
        if self.provider != target {
            return Ok(ProviderExtrasMergeOutcome::IgnoredProviderMismatch {
                extras_provider: self.provider,
                target,
            });
        }

        let body = body
            .as_object_mut()
            .ok_or(ProviderExtrasMergeError::BodyNotObject)?;
        body.extend(self.fields.clone());

        Ok(ProviderExtrasMergeOutcome::Merged)
    }
}

/// The observable result of attempting to merge provider extras.
#[must_use = "provider mismatch is reported through this outcome"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderExtrasMergeOutcome {
    /// All extra fields were merged into the request body.
    Merged,
    /// Extras were intentionally ignored because they belong to another
    /// provider.
    IgnoredProviderMismatch {
        /// Provider declared by the extras payload.
        extras_provider: ProviderId,
        /// Provider whose request body was being serialized.
        target: ProviderId,
    },
}

/// Errors produced while merging provider extras into a request body.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ProviderExtrasMergeError {
    /// A serialized provider request must be a JSON object before fields can
    /// be merged into it.
    #[error("provider extras can only be merged into a JSON object request body")]
    BodyNotObject,
}

#[cfg(test)]
mod tests {
    use super::{ProviderExtras, ProviderExtrasMergeError, ProviderExtrasMergeOutcome, ProviderId};
    use serde_json::{Map, json};

    #[test]
    fn merges_fields_when_provider_matches() {
        let extras = ProviderExtras {
            provider: ProviderId::Anthropic,
            fields: Map::from_iter([
                ("top_k".to_owned(), json!(25)),
                ("temperature".to_owned(), json!(0.25)),
            ]),
        };
        let mut body = json!({
            "model": "claude-test",
            "temperature": 1.0
        });

        let outcome = extras
            .merge_into(&mut body, ProviderId::Anthropic)
            .expect("merge matching provider extras");

        assert_eq!(outcome, ProviderExtrasMergeOutcome::Merged);
        assert_eq!(
            body,
            json!({
                "model": "claude-test",
                "temperature": 0.25,
                "top_k": 25
            })
        );
    }

    #[test]
    fn reports_and_ignores_fields_when_provider_mismatches() {
        let extras = ProviderExtras {
            provider: ProviderId::Anthropic,
            fields: Map::from_iter([("top_k".to_owned(), json!(25))]),
        };
        let mut body = json!({ "model": "gpt-test" });
        let original_body = body.clone();

        let outcome = extras
            .merge_into(&mut body, ProviderId::OpenAiResp)
            .expect("provider mismatch is an observable no-op");

        assert_eq!(
            outcome,
            ProviderExtrasMergeOutcome::IgnoredProviderMismatch {
                extras_provider: ProviderId::Anthropic,
                target: ProviderId::OpenAiResp,
            }
        );
        assert_eq!(body, original_body);
    }

    #[test]
    fn rejects_non_object_body_without_mutating_it() {
        let extras = ProviderExtras {
            provider: ProviderId::Anthropic,
            fields: Map::from_iter([("top_k".to_owned(), json!(25))]),
        };
        let mut body = json!(["not", "an", "object"]);
        let original_body = body.clone();

        let error = extras
            .merge_into(&mut body, ProviderId::Anthropic)
            .expect_err("matching extras require an object body");

        assert_eq!(error, ProviderExtrasMergeError::BodyNotObject);
        assert_eq!(body, original_body);
    }

    #[test]
    fn provider_extras_round_trip_for_every_provider_id() {
        for (provider, wire_name) in [
            (ProviderId::Anthropic, "anthropic"),
            (ProviderId::OpenAiResp, "open_ai_resp"),
        ] {
            let extras = ProviderExtras {
                provider,
                fields: Map::from_iter([("reasoning".to_owned(), json!({ "effort": "high" }))]),
            };

            let encoded = serde_json::to_value(&extras).expect("serialize provider extras");
            assert_eq!(encoded["provider"], json!(wire_name));
            let decoded: ProviderExtras =
                serde_json::from_value(encoded).expect("deserialize provider extras");

            assert_eq!(decoded, extras);
        }
    }
}
