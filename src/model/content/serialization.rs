//! Stable `ContentBlock` deserialization and legacy tool-result migration.

use super::{ContentBlock, ImageSource};
use crate::model::tool::ToolStatus;
use serde::{Deserialize, Deserializer, Serializer, de::Error as _, ser::SerializeMap};
use serde_json::{Map, Value};

/// Deserializes every content variant while giving tool results one migration
/// point for the historical `is_error` representation.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlockData {
    Text {
        text: String,
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
    Image {
        source: ImageSource,
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Vec<ContentBlock>,
        #[serde(default, deserialize_with = "deserialize_present")]
        status: Option<ToolStatus>,
        #[serde(default, deserialize_with = "deserialize_present")]
        is_error: Option<bool>,
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
    Thinking {
        text: String,
        #[serde(default)]
        signature: Option<String>,
        #[serde(default, flatten)]
        extra: Map<String, Value>,
    },
}

impl<'de> Deserialize<'de> for ContentBlock {
    /// Decodes the stable representation and migrates legacy tool-result
    /// booleans before exposing a complete normalized block.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let data = ContentBlockData::deserialize(deserializer)?;

        match data {
            ContentBlockData::Text { text, extra } => Ok(Self::Text { text, extra }),
            ContentBlockData::Image { source, extra } => Ok(Self::Image { source, extra }),
            ContentBlockData::ToolUse {
                id,
                name,
                input,
                extra,
            } => Ok(Self::ToolUse {
                id,
                name,
                input,
                extra,
            }),
            ContentBlockData::ToolResult {
                tool_use_id,
                content,
                status,
                is_error,
                extra,
            } => Ok(Self::ToolResult {
                tool_use_id,
                content,
                status: migrate_tool_status(status, is_error).map_err(D::Error::custom)?,
                extra,
            }),
            ContentBlockData::Thinking {
                text,
                signature,
                extra,
            } => Ok(Self::Thinking {
                text,
                signature,
                extra,
            }),
        }
    }
}

/// Distinguishes an absent migration field from a present `null`, so malformed
/// persisted data cannot silently acquire the legacy default.
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// Resolves the new four-state value and the historical boolean without ever
/// accepting two contradictory sources of truth.
fn migrate_tool_status(
    status: Option<ToolStatus>,
    is_error: Option<bool>,
) -> Result<ToolStatus, String> {
    let legacy_status = is_error.map(|is_error| {
        if is_error {
            ToolStatus::Error
        } else {
            ToolStatus::Ok
        }
    });

    match (status, legacy_status) {
        (Some(status), Some(legacy_status)) if status != legacy_status => Err(format!(
            "tool_result has conflicting `status` ({status:?}) and legacy `is_error` ({is_error:?})"
        )),
        (Some(status), _) => Ok(status),
        (None, Some(legacy_status)) => Ok(legacy_status),
        // Historical successful results omitted their default false flag.
        (None, None) => Ok(ToolStatus::Ok),
    }
}

/// Serializes only unmodeled tool-result fields. Modeled keys and the legacy
/// boolean can never override or duplicate the normalized status.
pub(super) fn serialize_tool_result_extra<S>(
    extra: &Map<String, Value>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let retained = extra
        .iter()
        .filter(|(key, _)| !is_modeled_tool_result_key(key))
        .collect::<Vec<_>>();
    let mut map = serializer.serialize_map(Some(retained.len()))?;
    for (key, value) in retained {
        map.serialize_entry(key, value)?;
    }
    map.end()
}

/// Identifies fields owned by the normalized tool-result schema rather than
/// its provider escape hatch.
fn is_modeled_tool_result_key(key: &str) -> bool {
    matches!(
        key,
        "type" | "tool_use_id" | "content" | "status" | "is_error"
    )
}
