//! Classified pending-turn failures and atomic retry behavior.

mod begin;
mod commit;
mod identity;
mod mapping;
mod results;

use super::{
    assistant_response, begin, call_id, committed_view, conversation, freeze_response, mapping,
    message_id, pending_view, text, tool_response, tool_use, turn_id, user,
};
use crate::{
    conversation::{
        CommitError, ContentBlockKind, ConversationError, PendingTurnError, PendingTurnPhase,
        ToolCallMapping, TurnMeta,
    },
    model::{
        content::{ContentBlock, ImageSource},
        message::{Message, Role},
        normalized::StopReason,
        tool::{ToolResponse, ToolStatus},
    },
};
use serde_json::Map;
