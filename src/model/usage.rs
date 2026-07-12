//! Token usage accounting types.

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};
use serde_json::{Map, Value};

/// Provider-neutral token accounting for one model response or stream segment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    /// Non-cached input tokens reported for the request.
    #[serde(default)]
    pub input: u32,
    /// Output tokens reported for the response.
    #[serde(default)]
    pub output: u32,
    /// Input tokens read from a provider-side prompt cache.
    #[serde(default)]
    pub cache_read: u32,
    /// Input tokens written to a provider-side prompt cache.
    #[serde(default)]
    pub cache_write: u32,
    /// Reasoning/thinking tokens reported separately by the provider.
    #[serde(default)]
    pub reasoning: u32,
    /// Provider-reported total token count, when one is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
    /// Provider-specific usage fields that this crate does not model yet.
    #[serde(default, flatten)]
    pub extra: Map<String, Value>,
}

impl Usage {
    /// Adds another usage record into this one for stream aggregation.
    pub fn merge(&mut self, other: Self) {
        self.input = checked_add(self.input, other.input, "input");
        self.output = checked_add(self.output, other.output, "output");
        self.cache_read = checked_add(self.cache_read, other.cache_read, "cache_read");
        self.cache_write = checked_add(self.cache_write, other.cache_write, "cache_write");
        self.reasoning = checked_add(self.reasoning, other.reasoning, "reasoning");
        self.total = match (self.total, other.total) {
            (Some(left), Some(right)) => Some(checked_add(left, right, "total")),
            (Some(total), None) | (None, Some(total)) => Some(total),
            (None, None) => None,
        };
        self.extra.extend(other.extra);
    }

    /// Computes a total from the normalized token columns.
    pub fn total_computed(&self) -> u32 {
        [
            ("input", self.input),
            ("output", self.output),
            ("cache_read", self.cache_read),
            ("cache_write", self.cache_write),
            ("reasoning", self.reasoning),
        ]
        .into_iter()
        .fold(0, |total, (field, value)| checked_add(total, value, field))
    }
}

impl<'de> Deserialize<'de> for Usage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut fields = Map::<String, Value>::deserialize(deserializer)?;

        let input = take_aliased_u32(&mut fields, &["input", "input_tokens", "prompt_tokens"])?;
        let output = take_aliased_u32(
            &mut fields,
            &["output", "output_tokens", "completion_tokens"],
        )?;
        let cache_read = take_aliased_u32(&mut fields, &["cache_read", "cache_read_input_tokens"])?;
        let cache_write =
            take_aliased_u32(&mut fields, &["cache_write", "cache_creation_input_tokens"])?;
        let reasoning = take_aliased_u32(&mut fields, &["reasoning", "reasoning_tokens"])?;
        let total = take_optional_aliased_u32(&mut fields, &["total", "total_tokens"])?;

        let mut usage = Self {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
            total,
            extra: fields,
        };

        usage.cache_read = merge_detail_alias(
            usage.cache_read,
            &mut usage.extra,
            "prompt_tokens_details",
            &["cached_tokens", "cache_read_tokens"],
            "cache_read",
        )?;
        usage.cache_write = merge_detail_alias(
            usage.cache_write,
            &mut usage.extra,
            "prompt_tokens_details",
            &["cache_creation_tokens", "cache_write_tokens"],
            "cache_write",
        )?;
        usage.reasoning = merge_detail_alias(
            usage.reasoning,
            &mut usage.extra,
            "completion_tokens_details",
            &["reasoning_tokens"],
            "reasoning",
        )?;
        usage.reasoning = merge_detail_alias(
            usage.reasoning,
            &mut usage.extra,
            "output_tokens_details",
            &["reasoning_tokens"],
            "reasoning",
        )?;

        Ok(usage)
    }
}

fn checked_add(left: u32, right: u32, field: &str) -> u32 {
    left.checked_add(right)
        .unwrap_or_else(|| panic!("usage {field} token count overflowed u32"))
}

fn merge_detail_alias<E>(
    current: u32,
    fields: &mut Map<String, Value>,
    detail_key: &str,
    aliases: &[&str],
    normalized_field: &str,
) -> Result<u32, E>
where
    E: DeError,
{
    let Some(details) = fields.remove(detail_key) else {
        return Ok(current);
    };

    let Value::Object(mut details) = details else {
        return Err(E::custom(format!(
            "usage field `{detail_key}` must be an object"
        )));
    };

    let merged = merge_aliased_value(current, &mut details, aliases, normalized_field)?;

    if !details.is_empty() {
        fields.insert(detail_key.to_owned(), Value::Object(details));
    }

    Ok(merged)
}

fn take_aliased_u32<E>(fields: &mut Map<String, Value>, aliases: &[&str]) -> Result<u32, E>
where
    E: DeError,
{
    take_optional_aliased_u32(fields, aliases).map(|value| value.unwrap_or_default())
}

fn take_optional_aliased_u32<E>(
    fields: &mut Map<String, Value>,
    aliases: &[&str],
) -> Result<Option<u32>, E>
where
    E: DeError,
{
    let mut found = None;

    for alias in aliases {
        let Some(value) = fields.remove(*alias) else {
            continue;
        };
        let value = value_to_u32::<E>(alias, value)?;
        if let Some(existing) = found
            && existing != value
        {
            return Err(E::custom(format!(
                "conflicting usage fields for `{}`",
                aliases[0]
            )));
        }
        found = Some(value);
    }

    Ok(found)
}

fn merge_aliased_value<E>(
    current: u32,
    fields: &mut Map<String, Value>,
    aliases: &[&str],
    normalized_field: &str,
) -> Result<u32, E>
where
    E: DeError,
{
    let Some(value) = take_optional_aliased_u32(fields, aliases)? else {
        return Ok(current);
    };

    if current != 0 && current != value {
        return Err(E::custom(format!(
            "conflicting usage fields for `{normalized_field}`"
        )));
    }

    Ok(value)
}

fn value_to_u32<E>(field: &str, value: Value) -> Result<u32, E>
where
    E: DeError,
{
    let Value::Number(number) = value else {
        return Err(E::custom(format!("usage field `{field}` must be a number")));
    };

    let Some(value) = number.as_u64() else {
        return Err(E::custom(format!(
            "usage field `{field}` must be a non-negative integer"
        )));
    };

    u32::try_from(value).map_err(|_| {
        E::custom(format!(
            "usage field `{field}` value {value} does not fit in u32"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::Usage;
    use serde_json::{Value, json};

    #[test]
    fn deserializes_anthropic_usage_fragment() {
        let usage: Usage = serde_json::from_value(json!({
            "input_tokens": 19,
            "output_tokens": 11,
            "cache_creation_input_tokens": 7,
            "cache_read_input_tokens": 5
        }))
        .expect("deserialize anthropic usage");

        assert_eq!(usage.input, 19);
        assert_eq!(usage.output, 11);
        assert_eq!(usage.cache_write, 7);
        assert_eq!(usage.cache_read, 5);
        assert_eq!(usage.reasoning, 0);
        assert_eq!(usage.total, None);
        assert!(usage.extra.is_empty());
    }

    #[test]
    fn deserializes_openai_usage_fragment() {
        let usage: Usage = serde_json::from_value(json!({
            "prompt_tokens": 23,
            "completion_tokens": 13,
            "total_tokens": 36,
            "prompt_tokens_details": {
                "cached_tokens": 3,
                "cache_creation_tokens": 2
            },
            "completion_tokens_details": {
                "reasoning_tokens": 8
            }
        }))
        .expect("deserialize openai usage");

        assert_eq!(usage.input, 23);
        assert_eq!(usage.output, 13);
        assert_eq!(usage.cache_read, 3);
        assert_eq!(usage.cache_write, 2);
        assert_eq!(usage.reasoning, 8);
        assert_eq!(usage.total, Some(36));
        assert!(usage.extra.is_empty());
    }

    #[test]
    fn deserializes_openai_response_usage_reasoning_alias() {
        let usage: Usage = serde_json::from_value(json!({
            "input_tokens": 31,
            "output_tokens": 17,
            "output_tokens_details": {
                "reasoning_tokens": 9
            }
        }))
        .expect("deserialize openai response usage");

        assert_eq!(usage.input, 31);
        assert_eq!(usage.output, 17);
        assert_eq!(usage.reasoning, 9);
    }

    #[test]
    fn keeps_unknown_fields_in_extra() {
        let usage: Usage = serde_json::from_value(json!({
            "input_tokens": 10,
            "provider_counter": 4,
            "completion_tokens_details": {
                "reasoning_tokens": 2,
                "accepted_prediction_tokens": 1
            }
        }))
        .expect("deserialize usage with extra fields");

        assert_eq!(usage.input, 10);
        assert_eq!(usage.reasoning, 2);
        assert_eq!(usage.extra.get("provider_counter"), Some(&json!(4)));
        assert_eq!(
            usage
                .extra
                .get("completion_tokens_details")
                .and_then(Value::as_object)
                .and_then(|details| details.get("accepted_prediction_tokens")),
            Some(&json!(1))
        );
    }

    #[test]
    fn usage_round_trips_through_canonical_fields() {
        let usage = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            reasoning: 5,
            total: Some(15),
            extra: [("provider".to_owned(), json!("openai"))]
                .into_iter()
                .collect(),
        };

        let json = serde_json::to_string(&usage).expect("serialize usage");
        let decoded: Usage = serde_json::from_str(&json).expect("deserialize usage");

        assert_eq!(decoded, usage);
    }

    #[test]
    fn merge_accumulates_usage_and_extra_fields() {
        let mut usage = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            reasoning: 5,
            total: Some(15),
            extra: [("first".to_owned(), json!(true))].into_iter().collect(),
        };
        usage.merge(Usage {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_write: 40,
            reasoning: 50,
            total: Some(150),
            extra: [("second".to_owned(), json!(true))].into_iter().collect(),
        });

        assert_eq!(usage.input, 11);
        assert_eq!(usage.output, 22);
        assert_eq!(usage.cache_read, 33);
        assert_eq!(usage.cache_write, 44);
        assert_eq!(usage.reasoning, 55);
        assert_eq!(usage.total, Some(165));
        assert_eq!(usage.extra.get("first"), Some(&json!(true)));
        assert_eq!(usage.extra.get("second"), Some(&json!(true)));
    }

    #[test]
    fn total_computed_sums_normalized_columns() {
        let usage = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            reasoning: 5,
            total: Some(99),
            extra: Default::default(),
        };

        assert_eq!(usage.total_computed(), 15);
    }

    #[test]
    fn rejects_conflicting_aliases() {
        let error = serde_json::from_value::<Usage>(json!({
            "input": 1,
            "input_tokens": 2
        }))
        .expect_err("conflicting aliases must fail");

        assert!(
            error
                .to_string()
                .contains("conflicting usage fields for `input`")
        );
    }
}
