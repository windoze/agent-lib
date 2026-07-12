//! Output-message content-part state and normalized text-block events.

use super::value::{content_block_id, object, required_string};
use crate::{
    client::ClientError,
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use serde_json::Value;

use super::super::invalid_stream;

/// Provider message content kinds represented as normalized text blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum PartKind {
    /// Normal assistant-visible output text.
    OutputText,
    /// Provider refusal text.
    Refusal,
}

impl PartKind {
    /// Returns the wire discriminator for this content kind.
    fn wire_name(self) -> &'static str {
        match self {
            Self::OutputText => "output_text",
            Self::Refusal => "refusal",
        }
    }

    /// Returns the matching delta event name for diagnostics.
    pub(super) fn delta_event_name(self) -> &'static str {
        match self {
            Self::OutputText => "response.output_text.delta",
            Self::Refusal => "response.refusal.delta",
        }
    }

    /// Returns the matching text-done event name for diagnostics.
    pub(super) fn done_event_name(self) -> &'static str {
        match self {
            Self::OutputText => "response.output_text.done",
            Self::Refusal => "response.refusal.done",
        }
    }
}

/// One message content part retained until `content_part.done`.
pub(super) struct ActivePart {
    kind: Option<PartKind>,
    block_id: Option<BlockId>,
    text: String,
    done_part: Option<Value>,
}

impl ActivePart {
    /// Starts a normalized text block for known content and passively tracks
    /// unsupported content for terminal consistency checks.
    pub(super) fn start(
        item_id: &str,
        content_index: u64,
        part: Value,
    ) -> Result<(Self, Vec<StreamEvent>), ClientError> {
        let fields = object(
            &part,
            &format!("message item `{item_id}` content {content_index}"),
        )?;
        let part_type = required_string(
            fields,
            "type",
            &format!("message item `{item_id}` content {content_index}"),
        )?;
        let kind = match part_type.as_str() {
            "output_text" => Some(PartKind::OutputText),
            "refusal" => Some(PartKind::Refusal),
            _ => None,
        };
        let Some(kind) = kind else {
            return Ok((
                Self {
                    kind: None,
                    block_id: None,
                    text: String::new(),
                    done_part: None,
                },
                Vec::new(),
            ));
        };
        let text_key = if kind == PartKind::OutputText {
            "text"
        } else {
            "refusal"
        };
        let text = required_string(
            fields,
            text_key,
            &format!("message item `{item_id}` content {content_index}"),
        )?;
        let block_id = content_block_id(item_id, content_index);
        let mut events = vec![StreamEvent::BlockStart {
            id: block_id.clone(),
            kind: BlockKind::Text,
        }];
        if !text.is_empty() {
            events.push(StreamEvent::BlockDelta {
                id: block_id.clone(),
                delta: Delta::Text(text.clone()),
            });
        }

        Ok((
            Self {
                kind: Some(kind),
                block_id: Some(block_id),
                text,
                done_part: None,
            },
            events,
        ))
    }

    /// Appends a delta after validating content kind and lifecycle.
    pub(super) fn push_delta(
        &mut self,
        item_id: &str,
        content_index: u64,
        delta: String,
        expected: PartKind,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_open(item_id, content_index, expected.delta_event_name())?;
        if self.kind != Some(expected) {
            return Err(invalid_stream(format!(
                "{} targeted {} content index {content_index} for item `{item_id}`",
                expected.delta_event_name(),
                self.kind_name()
            )));
        }
        self.text.push_str(&delta);
        Ok(vec![StreamEvent::BlockDelta {
            id: self
                .block_id
                .clone()
                .expect("known content part must have a block id"),
            delta: Delta::Text(delta),
        }])
    }

    /// Compares a text-done value with all accumulated fragments.
    pub(super) fn validate_text(
        &self,
        item_id: &str,
        content_index: u64,
        text: &str,
        expected: PartKind,
    ) -> Result<(), ClientError> {
        self.require_open(item_id, content_index, expected.done_event_name())?;
        if self.kind != Some(expected) {
            return Err(invalid_stream(format!(
                "{} targeted {} content index {content_index} for item `{item_id}`",
                expected.done_event_name(),
                self.kind_name()
            )));
        }
        if self.text != text {
            return Err(invalid_stream(format!(
                "{} for item `{item_id}` content {content_index} disagrees with accumulated deltas",
                expected.done_event_name()
            )));
        }
        Ok(())
    }

    /// Validates the complete part, retains it, and closes any normalized block.
    pub(super) fn finish(
        &mut self,
        item_id: &str,
        content_index: u64,
        part: Value,
    ) -> Result<Vec<StreamEvent>, ClientError> {
        self.require_open(item_id, content_index, "response.content_part.done")?;
        let fields = object(
            &part,
            &format!("message item `{item_id}` content {content_index}"),
        )?;
        let done_type = required_string(
            fields,
            "type",
            &format!("message item `{item_id}` content {content_index}"),
        )?;
        if let Some(kind) = self.kind {
            if done_type != kind.wire_name() {
                return Err(invalid_stream(format!(
                    "content_part.done type `{done_type}` disagrees with started type `{}` for item `{item_id}` content {content_index}",
                    kind.wire_name()
                )));
            }
            let text_key = if kind == PartKind::OutputText {
                "text"
            } else {
                "refusal"
            };
            let done_text = required_string(fields, text_key, "completed content part")?;
            if done_text != self.text {
                return Err(invalid_stream(format!(
                    "content_part.done for item `{item_id}` content {content_index} disagrees with accumulated text"
                )));
            }
        }

        self.done_part = Some(part);
        Ok(self
            .block_id
            .clone()
            .map(|id| vec![StreamEvent::BlockStop { id }])
            .unwrap_or_default())
    }

    /// Checks the final output-message item against `content_part.done`.
    pub(super) fn validate_done_value(
        &self,
        item_id: &str,
        content_index: u64,
        part: &Value,
    ) -> Result<(), ClientError> {
        let done = self.done_part.as_ref().ok_or_else(|| {
            invalid_stream(format!(
                "output_item.done arrived before item `{item_id}` content {content_index} completed"
            ))
        })?;
        if done != part {
            return Err(invalid_stream(format!(
                "output_item.done content {content_index} for item `{item_id}` disagrees with content_part.done"
            )));
        }
        Ok(())
    }

    /// Rejects deltas or duplicate completion after a part stops.
    fn require_open(
        &self,
        item_id: &str,
        content_index: u64,
        event: &str,
    ) -> Result<(), ClientError> {
        if self.done_part.is_some() {
            Err(invalid_stream(format!(
                "received {event} after item `{item_id}` content {content_index} completed"
            )))
        } else {
            Ok(())
        }
    }

    /// Names a content kind for mismatch diagnostics.
    fn kind_name(&self) -> &'static str {
        match self.kind {
            Some(PartKind::OutputText) => "output_text",
            Some(PartKind::Refusal) => "refusal",
            None => "unsupported",
        }
    }
}
