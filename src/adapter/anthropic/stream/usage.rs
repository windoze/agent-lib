//! Conversion of Anthropic cumulative usage snapshots into additive segments.

use super::{invalid_stream, wire::StreamUsage};
use crate::{client::ClientError, model::usage::Usage};

/// Last-seen Anthropic cumulative counters used to emit additive increments.
#[derive(Default)]
pub(super) struct UsageTracker {
    input: Option<u32>,
    output: Option<u32>,
    cache_read: Option<u32>,
    cache_write: Option<u32>,
}

impl UsageTracker {
    /// Converts one provider snapshot into a segment safe for `Usage::merge`.
    pub(super) fn incremental(&mut self, usage: StreamUsage) -> Result<Usage, ClientError> {
        Ok(Usage {
            input: counter_increment("input_tokens", usage.input_tokens, &mut self.input)?,
            output: counter_increment("output_tokens", usage.output_tokens, &mut self.output)?,
            cache_read: counter_increment(
                "cache_read_input_tokens",
                usage.cache_read_input_tokens,
                &mut self.cache_read,
            )?,
            cache_write: counter_increment(
                "cache_creation_input_tokens",
                usage.cache_creation_input_tokens,
                &mut self.cache_write,
            )?,
            reasoning: 0,
            total: None,
            extra: usage.extra,
        })
    }
}

/// Computes a non-negative increment from an optional cumulative counter.
fn counter_increment(
    field: &str,
    current: Option<u32>,
    previous: &mut Option<u32>,
) -> Result<u32, ClientError> {
    let Some(current) = current else {
        return Ok(0);
    };
    let baseline = previous.unwrap_or_default();
    let increment = current.checked_sub(baseline).ok_or_else(|| {
        invalid_stream(format!(
            "cumulative usage field `{field}` decreased from {baseline} to {current}"
        ))
    })?;
    *previous = Some(current);
    Ok(increment)
}
