//! Stream folding support for collecting events into complete responses.

use crate::{
    client::{ClientError, Response},
    model::{
        content::ContentBlock,
        message::{Message, Role},
        normalized::{Normalized, StopReason},
        usage::Usage,
    },
    stream::{BlockId, BlockKind, Delta, StreamEvent},
};
use futures::{Stream, StreamExt, pin_mut};
use serde_json::{Map, Value};
use std::collections::HashMap;
use thiserror::Error;

/// An error raised while validating or folding normalized stream events.
#[derive(Debug, Error)]
pub enum AccumulatorError {
    /// The provider emitted an explicit error event.
    #[error("stream reported an error: {0}")]
    Stream(#[source] ClientError),
    /// No message start event supplied the response role.
    #[error("stream ended without a message start event")]
    MissingMessageStart,
    /// No message stop event supplied the response stop reason.
    #[error("stream ended without a message stop event")]
    MissingMessageStop,
    /// More than one message start event was observed.
    #[error("stream contained more than one message start event")]
    DuplicateMessageStart,
    /// An event was observed after the message had already stopped.
    #[error("received `{0}` after the message stop event")]
    EventAfterMessageStop(&'static str),
    /// A block start reused an identifier that was already active or complete.
    #[error("block `{0}` was started more than once")]
    DuplicateBlock(BlockId),
    /// An event referred to a block identifier that was never started.
    #[error("block `{0}` was not started")]
    UnknownBlock(BlockId),
    /// A delta or stop event was emitted after its block had stopped.
    #[error("block `{0}` has already stopped")]
    BlockAlreadyStopped(BlockId),
    /// A block received a delta belonging to a different block kind.
    #[error("block `{id}` expects {expected} deltas but received a {actual} delta")]
    MismatchedDelta {
        /// Identifier of the block receiving the invalid delta.
        id: BlockId,
        /// Delta category required by the block kind.
        expected: &'static str,
        /// Delta category that was actually received.
        actual: &'static str,
    },
    /// A JSON delta arrived after complete tool input had been published.
    #[error("tool input for block `{0}` is already complete")]
    ToolInputAlreadyComplete(BlockId),
    /// A tool-input-available event targeted a non-tool block.
    #[error("block `{0}` cannot accept complete tool input")]
    UnexpectedToolInput(BlockId),
    /// More than one tool-input-available event targeted the same block.
    #[error("tool input for block `{0}` was published more than once")]
    DuplicateToolInput(BlockId),
    /// Accumulated tool input was not a complete JSON value.
    #[error("tool input for block `{id}` is invalid JSON: {source}")]
    InvalidToolInput {
        /// Identifier of the tool-input block containing invalid JSON.
        id: BlockId,
        /// JSON parser error explaining where the accumulated input failed.
        #[source]
        source: serde_json::Error,
    },
    /// The stream ended before a started block received its stop event.
    #[error("stream ended before block `{0}` stopped")]
    UnclosedBlock(BlockId),
}

/// An error returned while consuming a fallible stream of normalized events.
#[derive(Debug, Error)]
pub enum CollectError<E> {
    /// The source stream failed before yielding another normalized event.
    #[error("stream source failed: {0}")]
    Stream(E),
    /// A yielded event violated the accumulator contract.
    #[error(transparent)]
    Accumulator(#[from] AccumulatorError),
}

/// Stateful folder that reconstructs one complete response from stream events.
#[derive(Debug, Default)]
pub struct Accumulator {
    blocks: HashMap<BlockId, PartialBlock>,
    order: Vec<BlockId>,
    usage: Usage,
    role: Option<Role>,
    stop_reason: Option<Normalized<StopReason>>,
}

impl Accumulator {
    /// Creates an empty accumulator for one response stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates and folds one normalized event into the in-progress response.
    pub fn push(&mut self, event: StreamEvent) -> Result<(), AccumulatorError> {
        let event = match event {
            StreamEvent::Error(error) => return Err(AccumulatorError::Stream(error)),
            event => event,
        };

        if self.stop_reason.is_some() {
            return Err(AccumulatorError::EventAfterMessageStop(event_name(&event)));
        }

        match event {
            StreamEvent::MessageStart { role } => {
                if self.role.is_some() {
                    return Err(AccumulatorError::DuplicateMessageStart);
                }
                self.role = Some(role);
            }
            StreamEvent::BlockStart { id, kind } => self.start_block(id, kind)?,
            StreamEvent::BlockDelta { id, delta } => {
                let block = self
                    .blocks
                    .get_mut(&id)
                    .ok_or_else(|| AccumulatorError::UnknownBlock(id.clone()))?;
                block.push_delta(&id, delta)?;
            }
            StreamEvent::BlockStop { id } => {
                let block = self
                    .blocks
                    .get_mut(&id)
                    .ok_or_else(|| AccumulatorError::UnknownBlock(id.clone()))?;
                block.stop(&id)?;
            }
            StreamEvent::ToolInputAvailable { id, input } => {
                let block = self
                    .blocks
                    .get_mut(&id)
                    .ok_or_else(|| AccumulatorError::UnknownBlock(id.clone()))?;
                block.set_tool_input(&id, input)?;
            }
            StreamEvent::Usage(usage) => self.usage.merge(usage),
            StreamEvent::MessageStop { stop_reason } => {
                self.stop_reason = Some(stop_reason);
            }
            StreamEvent::Error(_) => unreachable!("error events return before state dispatch"),
        }

        Ok(())
    }

    /// Finishes folding and returns a complete response in block start order.
    pub fn finish(self) -> Result<Response, AccumulatorError> {
        let role = self.role.ok_or(AccumulatorError::MissingMessageStart)?;
        let stop_reason = self
            .stop_reason
            .ok_or(AccumulatorError::MissingMessageStop)?;
        let mut blocks = self.blocks;
        let mut content = Vec::with_capacity(self.order.len());

        for id in self.order {
            let partial = blocks
                .remove(&id)
                .ok_or_else(|| AccumulatorError::UnknownBlock(id.clone()))?;
            let (block, stopped) = partial.into_content(&id)?;
            if !stopped {
                return Err(AccumulatorError::UnclosedBlock(id));
            }
            content.push(block);
        }

        Ok(Response {
            message: Message { role, content },
            usage: self.usage,
            stop_reason,
            extra: Map::new(),
        })
    }

    /// Starts a new partial block and records its stable response order.
    fn start_block(&mut self, id: BlockId, kind: BlockKind) -> Result<(), AccumulatorError> {
        if self.blocks.contains_key(&id) {
            return Err(AccumulatorError::DuplicateBlock(id));
        }

        self.order.push(id.clone());
        self.blocks.insert(id, PartialBlock::new(kind));
        Ok(())
    }
}

/// Consumes a fallible event stream and folds it into one complete response.
///
/// Source errors remain distinguishable from normalized event or folding
/// errors through [`CollectError`].
pub async fn collect<S, E>(stream: S) -> Result<Response, CollectError<E>>
where
    S: Stream<Item = Result<StreamEvent, E>>,
{
    pin_mut!(stream);
    let mut accumulator = Accumulator::new();

    while let Some(event) = stream.next().await {
        accumulator.push(event.map_err(CollectError::Stream)?)?;
    }

    accumulator.finish().map_err(CollectError::Accumulator)
}

/// In-progress content for one stable block identifier.
#[derive(Debug)]
enum PartialBlock {
    Text {
        text: String,
        stopped: bool,
    },
    Reasoning {
        text: String,
        stopped: bool,
    },
    ToolInput {
        tool_name: String,
        tool_call_id: String,
        json: String,
        input: Option<Value>,
        input_available: bool,
        stopped: bool,
    },
}

impl PartialBlock {
    /// Creates empty accumulation state matching a block-start kind.
    fn new(kind: BlockKind) -> Self {
        match kind {
            BlockKind::Text => Self::Text {
                text: String::new(),
                stopped: false,
            },
            BlockKind::Reasoning => Self::Reasoning {
                text: String::new(),
                stopped: false,
            },
            BlockKind::ToolInput {
                tool_name,
                tool_call_id,
            } => Self::ToolInput {
                tool_name,
                tool_call_id,
                json: String::new(),
                input: None,
                input_available: false,
                stopped: false,
            },
        }
    }

    /// Appends a delta after checking block lifecycle and delta category.
    fn push_delta(&mut self, id: &BlockId, delta: Delta) -> Result<(), AccumulatorError> {
        if self.is_stopped() {
            return Err(AccumulatorError::BlockAlreadyStopped(id.clone()));
        }

        match (self, delta) {
            (Self::Text { text, .. }, Delta::Text(delta))
            | (Self::Reasoning { text, .. }, Delta::Reasoning(delta)) => {
                text.push_str(&delta);
                Ok(())
            }
            (
                Self::ToolInput {
                    json,
                    input_available,
                    ..
                },
                Delta::Json(delta),
            ) => {
                if *input_available {
                    return Err(AccumulatorError::ToolInputAlreadyComplete(id.clone()));
                }
                json.push_str(&delta);
                Ok(())
            }
            (block, delta) => Err(AccumulatorError::MismatchedDelta {
                id: id.clone(),
                expected: block.delta_name(),
                actual: delta.name(),
            }),
        }
    }

    /// Marks a block complete, parsing accumulated tool JSON when necessary.
    fn stop(&mut self, id: &BlockId) -> Result<(), AccumulatorError> {
        if self.is_stopped() {
            return Err(AccumulatorError::BlockAlreadyStopped(id.clone()));
        }

        if let Self::ToolInput {
            json,
            input,
            stopped,
            ..
        } = self
        {
            if input.is_none() {
                *input = Some(parse_tool_input(id, json)?);
            }
            *stopped = true;
        } else {
            self.set_stopped();
        }

        Ok(())
    }

    /// Stores authoritative complete tool input from an available event.
    fn set_tool_input(
        &mut self,
        id: &BlockId,
        complete_input: Value,
    ) -> Result<(), AccumulatorError> {
        let Self::ToolInput {
            input,
            input_available,
            ..
        } = self
        else {
            return Err(AccumulatorError::UnexpectedToolInput(id.clone()));
        };

        if *input_available {
            return Err(AccumulatorError::DuplicateToolInput(id.clone()));
        }

        *input = Some(complete_input);
        *input_available = true;
        Ok(())
    }

    /// Converts finalized partial data into its complete content-block shape.
    fn into_content(self, id: &BlockId) -> Result<(ContentBlock, bool), AccumulatorError> {
        match self {
            Self::Text { text, stopped } => Ok((
                ContentBlock::Text {
                    text,
                    extra: Map::new(),
                },
                stopped,
            )),
            Self::Reasoning { text, stopped } => Ok((
                ContentBlock::Thinking {
                    text,
                    signature: None,
                    extra: Map::new(),
                },
                stopped,
            )),
            Self::ToolInput {
                tool_name,
                tool_call_id,
                json,
                input,
                stopped,
                ..
            } => {
                let input = match input {
                    Some(input) => input,
                    None => parse_tool_input(id, &json)?,
                };

                Ok((
                    ContentBlock::ToolUse {
                        id: tool_call_id,
                        name: tool_name,
                        input,
                        extra: Map::new(),
                    },
                    stopped,
                ))
            }
        }
    }

    /// Returns whether this block has received its stop event.
    fn is_stopped(&self) -> bool {
        match self {
            Self::Text { stopped, .. }
            | Self::Reasoning { stopped, .. }
            | Self::ToolInput { stopped, .. } => *stopped,
        }
    }

    /// Marks a non-tool block stopped after the shared lifecycle check.
    fn set_stopped(&mut self) {
        match self {
            Self::Text { stopped, .. } | Self::Reasoning { stopped, .. } => *stopped = true,
            Self::ToolInput { .. } => unreachable!("tool blocks are stopped after JSON parsing"),
        }
    }

    /// Names the only delta category accepted by this partial block.
    fn delta_name(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Reasoning { .. } => "reasoning",
            Self::ToolInput { .. } => "json",
        }
    }
}

impl Delta {
    /// Names a delta category for protocol diagnostics.
    fn name(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Json(_) => "json",
            Self::Reasoning(_) => "reasoning",
        }
    }
}

/// Parses accumulated tool JSON only at a complete-input boundary.
fn parse_tool_input(id: &BlockId, json: &str) -> Result<Value, AccumulatorError> {
    serde_json::from_str(json).map_err(|source| AccumulatorError::InvalidToolInput {
        id: id.clone(),
        source,
    })
}

/// Returns a stable event name for lifecycle error messages.
fn event_name(event: &StreamEvent) -> &'static str {
    match event {
        StreamEvent::MessageStart { .. } => "message_start",
        StreamEvent::BlockStart { .. } => "block_start",
        StreamEvent::BlockDelta { .. } => "block_delta",
        StreamEvent::BlockStop { .. } => "block_stop",
        StreamEvent::ToolInputAvailable { .. } => "tool_input_available",
        StreamEvent::Usage(_) => "usage",
        StreamEvent::MessageStop { .. } => "message_stop",
        StreamEvent::Error(_) => "error",
    }
}

#[cfg(test)]
mod tests;
