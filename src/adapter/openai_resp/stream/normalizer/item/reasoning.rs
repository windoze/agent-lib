//! Indexed reasoning text accumulation and normalized replay.

use super::value::{object, required_string};
use crate::{
    client::ClientError,
    stream::{BlockId, Delta, StreamEvent},
};
use serde_json::Value;
use std::collections::BTreeMap;

use super::super::invalid_stream;

/// Indexed reasoning parts with fragment order retained for normalized replay.
#[derive(Default)]
pub(super) struct IndexedText {
    parts: BTreeMap<u64, String>,
    fragments: Vec<(u64, String)>,
    last_index: Option<u64>,
}

impl IndexedText {
    /// Builds indexed state from complete reasoning `content` or `summary`
    /// arrays found in an item placeholder or done value.
    pub(super) fn from_parts(
        parts: &[Value],
        field: &str,
        item_id: &str,
    ) -> Result<Self, ClientError> {
        let mut indexed = Self::default();
        for (index, part) in parts.iter().enumerate() {
            let index = u64::try_from(index).map_err(|_| {
                invalid_stream(format!(
                    "reasoning item `{item_id}` field `{field}` is too large"
                ))
            })?;
            let fields = object(
                part,
                &format!("reasoning item `{item_id}` field `{field}` entry {index}"),
            )?;
            let text = required_string(
                fields,
                "text",
                &format!("reasoning item `{item_id}` field `{field}` entry {index}"),
            )?;
            indexed.push(index, text)?;
        }
        Ok(indexed)
    }

    /// Appends one fragment, requiring nondecreasing part indices.
    pub(super) fn push(&mut self, index: u64, fragment: String) -> Result<String, ClientError> {
        if let Some(last) = self.last_index
            && index < last
        {
            return Err(invalid_stream(format!(
                "reasoning part index decreased from {last} to {index}"
            )));
        }
        let is_new = !self.parts.contains_key(&index);
        let needs_separator = is_new && self.parts.values().any(|text| !text.is_empty());
        self.parts.entry(index).or_default().push_str(&fragment);
        self.fragments.push((index, fragment.clone()));
        self.last_index = Some(index);

        if needs_separator {
            Ok(format!("\n{fragment}"))
        } else {
            Ok(fragment)
        }
    }

    /// Validates one text-done value against accumulated fragments.
    pub(super) fn validate(
        &self,
        index: u64,
        text: &str,
        item_id: &str,
        field: &str,
    ) -> Result<(), ClientError> {
        let accumulated = self.parts.get(&index).ok_or_else(|| {
            invalid_stream(format!(
                "reasoning done referenced unstarted `{field}` index {index} for item `{item_id}`"
            ))
        })?;
        if accumulated != text {
            return Err(invalid_stream(format!(
                "reasoning done for item `{item_id}` `{field}` index {index} disagrees with accumulated deltas"
            )));
        }
        Ok(())
    }

    /// Joins complete parts using the same rule as non-streaming conversion.
    pub(super) fn joined(&self) -> String {
        self.parts
            .values()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Reports whether no raw content was observed.
    pub(super) fn is_empty(&self) -> bool {
        self.parts.values().all(String::is_empty)
    }

    /// Replays retained fragments as normalized reasoning deltas, inserting
    /// separators between distinct provider parts.
    pub(super) fn replay(
        &self,
        block_id: &BlockId,
        into_delta: impl Fn(String) -> Delta,
    ) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let mut last_index = None;
        for (index, fragment) in &self.fragments {
            let mut value = fragment.clone();
            if let Some(last) = last_index
                && *index != last
            {
                value.insert(0, '\n');
            }
            events.push(StreamEvent::BlockDelta {
                id: block_id.clone(),
                delta: into_delta(value),
            });
            last_index = Some(*index);
        }
        events
    }
}
