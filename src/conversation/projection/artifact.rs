//! Projection artifacts and runtime strategy references.
//!
//! Artifacts are persisted data produced outside Conversation Core. They carry
//! complete Client messages to render later plus provenance proving which raw
//! Turn range and strategy created them.

use super::CheckedTurnRange;
use crate::{
    conversation::{ArtifactId, ProjectionError},
    model::{message::Message, usage::Usage},
};
use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};
use serde_json::{Map, Value};
use std::fmt;

/// Serializable reference to an external compaction strategy implementation.
///
/// This is data, not a trait object or registry handle. Runtime code can use
/// the stable `name` and `version` to find the real summarizer later, while
/// Conversation snapshots remain free of client handles and closures.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyRef {
    name: String,
    version: String,
}

impl StrategyRef {
    /// Creates a serializable strategy reference from caller-owned strings.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }

    /// Returns the externally meaningful strategy name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the externally meaningful strategy version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

impl fmt::Display for StrategyRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.name, self.version)
    }
}

/// Token accounting attached to one compaction artifact.
///
/// The two records use the crate's provider-neutral [`Usage`] model. `before`
/// describes the source range before compaction; `after` describes the
/// artifact rendering that replaces it in a projected context.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenAccounting {
    before: Usage,
    after: Usage,
}

impl TokenAccounting {
    /// Creates accounting for one compaction result.
    #[must_use]
    pub fn new(before: Usage, after: Usage) -> Self {
        Self { before, after }
    }

    /// Returns token usage for the covered raw input before compaction.
    #[must_use]
    pub const fn before(&self) -> &Usage {
        &self.before
    }

    /// Returns token usage for the rendered artifact after compaction.
    #[must_use]
    pub const fn after(&self) -> &Usage {
        &self.after
    }
}

/// Provenance for a projection artifact.
///
/// Provenance stores stable Turn anchors through [`CheckedTurnRange`], the
/// strategy that produced the artifact, token accounting, and optional
/// extensible data. It never rewrites or annotates raw messages in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactProvenance {
    input_range: CheckedTurnRange,
    produced_by: StrategyRef,
    tokens: TokenAccounting,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    extra: Map<String, Value>,
}

impl ArtifactProvenance {
    /// Creates provenance for an externally produced artifact.
    #[must_use]
    pub fn new(
        input_range: CheckedTurnRange,
        produced_by: StrategyRef,
        tokens: TokenAccounting,
        extra: Map<String, Value>,
    ) -> Self {
        Self {
            input_range,
            produced_by,
            tokens,
            extra,
        }
    }

    /// Returns the stable raw Turn range summarized by the artifact.
    #[must_use]
    pub const fn input_range(&self) -> &CheckedTurnRange {
        &self.input_range
    }

    /// Returns the strategy reference recorded by the producer.
    #[must_use]
    pub const fn produced_by(&self) -> &StrategyRef {
        &self.produced_by
    }

    /// Returns token accounting for the compaction.
    #[must_use]
    pub const fn tokens(&self) -> &TokenAccounting {
        &self.tokens
    }

    /// Returns optional unmodeled provenance fields.
    #[must_use]
    pub const fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }
}

/// Serializable projection artifact rendered in place of covered raw Turns.
///
/// The artifact owns complete Client [`Message`] values. It does not own or
/// mutate any [`ConversationMessage`](crate::conversation::ConversationMessage)
/// in raw history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    id: ArtifactId,
    messages: Vec<Message>,
    provenance: ArtifactProvenance,
}

impl Artifact {
    /// Creates an artifact with at least one complete Client message.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::EmptyArtifactMessages`] when `messages` is
    /// empty. Complete-message validity otherwise comes from the Client
    /// [`Message`] type; raw Conversation messages are never modified.
    pub fn new(
        id: ArtifactId,
        messages: Vec<Message>,
        provenance: ArtifactProvenance,
    ) -> Result<Self, ProjectionError> {
        let artifact = Self {
            id,
            messages,
            provenance,
        };
        artifact.validate_messages()?;
        Ok(artifact)
    }

    /// Returns this artifact's caller-supplied identity.
    #[must_use]
    pub const fn id(&self) -> ArtifactId {
        self.id
    }

    /// Returns complete Client messages used when the artifact is rendered.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Returns immutable provenance for this artifact.
    #[must_use]
    pub const fn provenance(&self) -> &ArtifactProvenance {
        &self.provenance
    }

    /// Re-checks serde-restored artifacts before they enter a Projection.
    pub(crate) fn validate_messages(&self) -> Result<(), ProjectionError> {
        if self.messages.is_empty() {
            return Err(ProjectionError::EmptyArtifactMessages {
                artifact_id: self.id,
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Artifact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ArtifactData {
            id: ArtifactId,
            messages: Vec<Message>,
            provenance: ArtifactProvenance,
        }

        let data = ArtifactData::deserialize(deserializer)?;
        Self::new(data.id, data.messages, data.provenance).map_err(D::Error::custom)
    }
}
