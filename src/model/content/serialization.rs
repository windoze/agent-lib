//! Stable `ContentBlock` deserialization and legacy tool-result migration.

use super::{ContentBlock, ImageSource};
use crate::model::tool::ToolStatus;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::Error as _};
use serde_json::{Map, Value};

const KNOWN_CONTENT_TYPES: &[&str] = &["text", "image", "tool_use", "tool_result", "thinking"];

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
        let value = Value::deserialize(deserializer)?;
        let Some(type_name) = value.get("type").and_then(Value::as_str) else {
            let data = ContentBlockData::deserialize(value).map_err(D::Error::custom)?;
            return content_block_from_data(data).map_err(D::Error::custom);
        };
        if !KNOWN_CONTENT_TYPES.contains(&type_name) {
            return Ok(Self::Unknown {
                type_name: Some(type_name.to_owned()),
                raw: value,
            });
        }

        let data = ContentBlockData::deserialize(value).map_err(D::Error::custom)?;
        content_block_from_data(data).map_err(D::Error::custom)
    }
}

impl Serialize for ContentBlock {
    /// Encodes known blocks in the normalized schema and writes unknown raw
    /// provider blocks back best-effort without promising exact fidelity.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = match self {
            Self::Text { text, extra } => {
                let mut fields = extra.clone();
                insert_string(&mut fields, "type", "text");
                insert_string(&mut fields, "text", text);
                Value::Object(fields)
            }
            Self::Image { source, extra } => {
                let mut fields = extra.clone();
                insert_string(&mut fields, "type", "image");
                fields.insert(
                    "source".to_owned(),
                    serde_json::to_value(source).map_err(S::Error::custom)?,
                );
                Value::Object(fields)
            }
            Self::ToolUse {
                id,
                name,
                input,
                extra,
            } => {
                let mut fields = extra.clone();
                insert_string(&mut fields, "type", "tool_use");
                insert_string(&mut fields, "id", id);
                insert_string(&mut fields, "name", name);
                fields.insert("input".to_owned(), input.clone());
                Value::Object(fields)
            }
            Self::ToolResult {
                tool_use_id,
                content,
                status,
                extra,
            } => {
                let mut fields = extra
                    .iter()
                    .filter(|(key, _)| !is_modeled_tool_result_key(key))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<Map<_, _>>();
                insert_string(&mut fields, "type", "tool_result");
                insert_string(&mut fields, "tool_use_id", tool_use_id);
                fields.insert(
                    "content".to_owned(),
                    serde_json::to_value(content).map_err(S::Error::custom)?,
                );
                fields.insert(
                    "status".to_owned(),
                    serde_json::to_value(status).map_err(S::Error::custom)?,
                );
                Value::Object(fields)
            }
            Self::Thinking {
                text,
                signature,
                extra,
            } => {
                let mut fields = extra.clone();
                insert_string(&mut fields, "type", "thinking");
                insert_string(&mut fields, "text", text);
                if let Some(signature) = signature {
                    insert_string(&mut fields, "signature", signature);
                } else {
                    fields.remove("signature");
                }
                Value::Object(fields)
            }
            Self::Unknown { raw, .. } => raw.clone(),
        };

        value.serialize(serializer)
    }
}

fn content_block_from_data(data: ContentBlockData) -> Result<ContentBlock, String> {
    match data {
        ContentBlockData::Text { text, extra } => Ok(ContentBlock::Text { text, extra }),
        ContentBlockData::Image { source, extra } => Ok(ContentBlock::Image { source, extra }),
        ContentBlockData::ToolUse {
            id,
            name,
            input,
            extra,
        } => Ok(ContentBlock::ToolUse {
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
        } => Ok(ContentBlock::ToolResult {
            tool_use_id,
            content,
            status: migrate_tool_status(status, is_error)?,
            extra,
        }),
        ContentBlockData::Thinking {
            text,
            signature,
            extra,
        } => Ok(ContentBlock::Thinking {
            text,
            signature,
            extra,
        }),
    }
}

fn insert_string(fields: &mut Map<String, Value>, key: &str, value: &str) {
    fields.insert(key.to_owned(), Value::String(value.to_owned()));
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

/// Identifies fields owned by the normalized tool-result schema rather than
/// its provider escape hatch.
fn is_modeled_tool_result_key(key: &str) -> bool {
    matches!(
        key,
        "type" | "tool_use_id" | "content" | "status" | "is_error"
    )
}
