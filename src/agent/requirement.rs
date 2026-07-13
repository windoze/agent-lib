//! Addressable effect requirements and their return path.
//!
//! In the effect model an Agent step is a pure state machine: it never performs
//! IO. Instead it *reifies* the IO it needs into a [`Requirement`] — an
//! addressable request the driver fulfills out of band. Each requirement is
//! stamped with a [`RequirementId`] (host-supplied, never generated here) and an
//! [`AgentPath`] origin so its fulfilled result can be routed back to the exact
//! stuck step, even across a hierarchy of nested machines.
//!
//! # Persistence boundary
//!
//! The types split along a persistable / runtime line, mirrored by their derives:
//!
//! - **Persistable requirement description** — [`Requirement`],
//!   [`RequirementKind`], [`RequirementId`], [`AgentPath`], and [`AgentSlot`]
//!   are pure data with `serde` support. A driver can serialize an outstanding
//!   requirement, restore it in another process, and re-register it.
//! - **Runtime resolution** — [`RequirementResult`] and
//!   [`RequirementResolution`] carry live results such as
//!   [`Result<Response, ClientError>`](crate::client::Response) and
//!   [`ToolRuntimeError`], some of which are runtime errors that are
//!   intentionally *not* persisted. They deliberately do **not** derive `serde`;
//!   a cross-process driver reconstructs the pending registry from the
//!   persisted [`RequirementId`]s in the machine cursor and re-fulfills.
//!
//! This module only defines data. It is not wired into any driver yet; the
//! sans-io `step` that emits requirements and consumes resolutions lands in a
//! later milestone.

use crate::{
    agent::{
        AgentError, AgentId, LlmStepMode, ToolSetRef,
        interaction::{Interaction, InteractionError, InteractionResponse},
        tool::ToolRuntimeError,
    },
    client::{ChatRequest, ClientError, Response},
    conversation::ToolCallId,
    model::tool::{ToolCall, ToolResponse},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, str::FromStr};
use thiserror::Error;
use uuid::Uuid;

/// Opaque identity for one reified requirement, used for return-path routing.
///
/// Like every Agent-layer identity, the wrapped UUID is supplied by the host
/// (see [`RequirementIds`]); this library never generates one. The value is
/// nominally distinct and serializes transparently as its UUID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[repr(transparent)]
pub struct RequirementId(Uuid);

impl RequirementId {
    /// Creates a `RequirementId` from an externally supplied UUID.
    #[must_use]
    pub const fn new(value: Uuid) -> Self {
        Self(value)
    }

    /// Parses an externally supplied UUID into a `RequirementId`.
    ///
    /// # Errors
    ///
    /// Returns [`uuid::Error`] when `value` is not a UUID accepted by the
    /// `uuid` parser.
    pub fn parse_str(value: &str) -> Result<Self, uuid::Error> {
        Uuid::parse_str(value).map(Self::new)
    }

    /// Returns the externally supplied UUID inside this `RequirementId`.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }

    /// Consumes this `RequirementId` and returns its UUID.
    #[must_use]
    pub const fn into_uuid(self) -> Uuid {
        self.0
    }
}

impl FromStr for RequirementId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_str(value)
    }
}

impl fmt::Display for RequirementId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// Discriminant identifying which family a [`RequirementKind`] or
/// [`RequirementResult`] belongs to.
///
/// The tag drives return-path type alignment ([`RequirementKind::accepts`]) and
/// lets a host allocate ids per requirement family via [`RequirementIds`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKindTag {
    /// One LLM generation.
    Llm,
    /// One tool execution.
    Tool,
    /// One interaction with the "user" (approval / question / choice).
    Interaction,
    /// Deriving and driving a child agent.
    Subagent,
    /// Resolving a live tool registry for a queued tool-set reconfiguration.
    Reconfig,
}

impl fmt::Display for RequirementKindTag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Llm => "llm",
            Self::Tool => "tool",
            Self::Interaction => "interaction",
            Self::Subagent => "subagent",
            Self::Reconfig => "reconfig",
        };
        formatter.write_str(text)
    }
}

/// Caller-supplied identity source for reified requirements.
///
/// This mirrors [`crate::agent::ToolExecutionIds`]: the library deliberately
/// does not generate ids. Implementations should draw from host-provided
/// queues, database rows, deterministic fixtures, or another external
/// allocation boundary.
pub trait RequirementIds: Send + Sync + fmt::Debug {
    /// Returns the next requirement id for a requirement of `kind_tag`.
    ///
    /// # Errors
    ///
    /// Returns [`RequirementError::IdUnavailable`] when no stable id is
    /// available for the requested family.
    fn next_requirement_id(
        &self,
        kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError>;
}

/// Identity provider that never supplies a requirement id.
///
/// Useful as a default for machines that are constructed before a real id
/// source is wired in; every call returns a classified
/// [`RequirementError::IdUnavailable`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRequirementIds;

impl RequirementIds for NoRequirementIds {
    fn next_requirement_id(
        &self,
        kind_tag: RequirementKindTag,
    ) -> Result<RequirementId, RequirementError> {
        Err(RequirementError::IdUnavailable { kind: kind_tag })
    }
}

/// Slot of a child machine inside its parent, one hop along an [`AgentPath`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[repr(transparent)]
pub struct AgentSlot(u32);

impl AgentSlot {
    /// Creates a slot from an externally assigned child index.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Returns the child index this slot addresses.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for AgentSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// Path from the root machine to the node that emitted a requirement.
///
/// The root machine has the empty path. During the stage-0 single-machine
/// migration every requirement originates at the root, so the path is always
/// empty; the type is introduced now so signatures do not change when nested
/// machines land in stage 4.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentPath(Vec<AgentSlot>);

impl AgentPath {
    /// Returns the root path (empty).
    #[must_use]
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    /// Creates a path from an ordered list of slots, root first.
    #[must_use]
    pub const fn from_slots(slots: Vec<AgentSlot>) -> Self {
        Self(slots)
    }

    /// Returns `true` when this is the root path.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the number of hops from the root.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` when the path has no slots (the root).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the slots along the path, root first.
    #[must_use]
    pub fn slots(&self) -> &[AgentSlot] {
        &self.0
    }

    /// Returns a new path extended by one child slot.
    #[must_use]
    pub fn child(&self, slot: AgentSlot) -> Self {
        let mut slots = self.0.clone();
        slots.push(slot);
        Self(slots)
    }

    /// Appends one child slot in place.
    pub fn push(&mut self, slot: AgentSlot) {
        self.0.push(slot);
    }

    /// Iterates the slots along the path, root first.
    pub fn iter(&self) -> std::slice::Iter<'_, AgentSlot> {
        self.0.iter()
    }
}

/// Stage-0 placeholder reference to a subagent specification.
///
/// Stage 4 (task M5) refines this into the real subagent spec addressing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentSpecRef(pub AgentId);

/// Stage-0 placeholder for a subagent's produced output.
///
/// Stage 4 (task M5) refines this into the real subagent result payload. It is
/// a runtime value (part of [`RequirementResult`]) and therefore is not
/// required to be persistable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentOutput {
    /// Opaque summary carried until the real subagent result model lands.
    pub summary: String,
}

/// One reified effect the machine is stuck on, awaiting external fulfillment.
///
/// This is the persistable *description* of a request. Its fulfilled result is
/// delivered separately through a [`RequirementResolution`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    /// Unique identity used to route the fulfilled result back (return path).
    pub id: RequirementId,
    /// Path of the emitting node in the machine hierarchy (root is empty).
    #[serde(default)]
    pub origin: AgentPath,
    /// What the machine needs fulfilled.
    pub kind: RequirementKind,
}

impl Requirement {
    /// Creates a requirement stamped with an id, origin path, and kind.
    #[must_use]
    pub const fn new(id: RequirementId, origin: AgentPath, kind: RequirementKind) -> Self {
        Self { id, origin, kind }
    }

    /// Creates a requirement originating at the root machine.
    #[must_use]
    pub fn at_root(id: RequirementId, kind: RequirementKind) -> Self {
        Self::new(id, AgentPath::root(), kind)
    }

    /// Returns the family this requirement belongs to.
    #[must_use]
    pub fn tag(&self) -> RequirementKindTag {
        self.kind.tag()
    }

    /// Checks that `resolution` targets this requirement and carries a
    /// type-aligned result.
    ///
    /// # Errors
    ///
    /// Returns [`RequirementError::IdMismatch`] when the resolution addresses a
    /// different requirement, or [`RequirementError::ResultKindMismatch`] when
    /// the result family does not match this requirement's family.
    pub fn accepts_resolution(
        &self,
        resolution: &RequirementResolution,
    ) -> Result<(), RequirementError> {
        if resolution.id != self.id {
            return Err(RequirementError::IdMismatch {
                expected: self.id,
                actual: resolution.id,
            });
        }
        self.kind.accepts(&resolution.result)
    }
}

/// What a [`Requirement`] needs fulfilled. Payloads reuse existing types.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    /// One LLM generation; payload reuses [`ChatRequest`].
    NeedLlm {
        /// Provider-neutral request to send to the model.
        request: ChatRequest,
        /// Transport mode (streaming / non-streaming) for the generation.
        mode: LlmStepMode,
    },
    /// One tool execution; ids and call reuse existing Conversation types.
    NeedTool {
        /// Framework tool-call identity paired through Conversation.
        call_id: ToolCallId,
        /// Provider-neutral tool call selected by the model.
        call: ToolCall,
    },
    /// One interaction with the "user" (generalizes approval).
    NeedInteraction {
        /// The interaction to present externally.
        request: Interaction,
    },
    /// Deriving and driving a child agent (the only scope-deepening kind).
    NeedSubagent {
        /// Reference to the child agent's static specification.
        spec_ref: AgentSpecRef,
        /// Brief presented to the child agent as an interaction.
        brief: Interaction,
        /// Optional JSON schema the child result must conform to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_schema: Option<Value>,
    },
    /// Resolving a live tool registry for a queued tool-set reconfiguration.
    ///
    /// Emitted at a turn boundary when a queued reconfiguration changes the
    /// active tool set. The driver resolves `tool_set` to a live registry,
    /// validates its declarations against the requested set, swaps it in, and
    /// confirms with a [`RequirementResult::Reconfig`]. The machine itself holds
    /// no registry, so the swap stays a driver-side side effect.
    NeedReconfigRegistry {
        /// The queued tool set whose live registry the driver must resolve.
        tool_set: ToolSetRef,
    },
}

impl RequirementKind {
    /// Returns the family this kind belongs to.
    #[must_use]
    pub const fn tag(&self) -> RequirementKindTag {
        match self {
            Self::NeedLlm { .. } => RequirementKindTag::Llm,
            Self::NeedTool { .. } => RequirementKindTag::Tool,
            Self::NeedInteraction { .. } => RequirementKindTag::Interaction,
            Self::NeedSubagent { .. } => RequirementKindTag::Subagent,
            Self::NeedReconfigRegistry { .. } => RequirementKindTag::Reconfig,
        }
    }

    /// Checks that `result` is type-aligned with this requirement kind.
    ///
    /// A `NeedLlm` requirement only accepts an
    /// [`RequirementResult::Llm`] result, and so on for each family. For a
    /// `NeedInteraction` requirement, the carried [`InteractionResponse`] is
    /// additionally validated against the [`Interaction`] request (choice range,
    /// approval step/call match, response family).
    ///
    /// # Errors
    ///
    /// Returns [`RequirementError::ResultKindMismatch`] when the result family
    /// does not match this requirement's family, or
    /// [`RequirementError::Interaction`] when an interaction response fails its
    /// request-specific check.
    pub fn accepts(&self, result: &RequirementResult) -> Result<(), RequirementError> {
        let expected = self.tag();
        let actual = result.tag();
        if expected != actual {
            return Err(RequirementError::ResultKindMismatch { expected, actual });
        }
        if let (Self::NeedInteraction { request }, RequirementResult::Interaction(response)) =
            (self, result)
        {
            request
                .accepts_response(response)
                .map_err(RequirementError::Interaction)?;
        }
        Ok(())
    }
}

/// Fulfilled result for one requirement, delivered back on the return path.
///
/// This is the runtime half: it carries live values and runtime errors
/// ([`ClientError`], [`ToolRuntimeError`], [`AgentError`]) and is intentionally
/// not persistable. See the [module docs](self#persistence-boundary).
#[derive(Clone, Debug)]
pub enum RequirementResult {
    /// Result of an LLM generation.
    Llm(Result<Response, ClientError>),
    /// Result of a tool execution.
    Tool(Result<ToolResponse, ToolRuntimeError>),
    /// Result of an interaction with the "user".
    Interaction(InteractionResponse),
    /// Result of a driven subagent.
    Subagent(Result<SubagentOutput, AgentError>),
    /// Result of resolving a live registry for a tool-set reconfiguration.
    ///
    /// `Ok(())` confirms the driver swapped in a validated registry for the
    /// requested tool set; `Err` reports a resolution or declaration-mismatch
    /// failure, which fails the parked turn boundary.
    Reconfig(Result<(), ToolRuntimeError>),
}

impl RequirementResult {
    /// Returns the family this result belongs to.
    #[must_use]
    pub const fn tag(&self) -> RequirementKindTag {
        match self {
            Self::Llm(_) => RequirementKindTag::Llm,
            Self::Tool(_) => RequirementKindTag::Tool,
            Self::Interaction(_) => RequirementKindTag::Interaction,
            Self::Subagent(_) => RequirementKindTag::Subagent,
            Self::Reconfig(_) => RequirementKindTag::Reconfig,
        }
    }
}

/// A requirement result addressed back to the requirement it fulfills.
///
/// Runtime half; not persistable (see the [module docs](self#persistence-boundary)).
#[derive(Clone, Debug)]
pub struct RequirementResolution {
    /// Identity of the requirement being fulfilled.
    pub id: RequirementId,
    /// The fulfilled result.
    pub result: RequirementResult,
}

impl RequirementResolution {
    /// Creates a resolution addressed to `id`.
    #[must_use]
    pub const fn new(id: RequirementId, result: RequirementResult) -> Self {
        Self { id, result }
    }

    /// Returns the family of the carried result.
    #[must_use]
    pub const fn tag(&self) -> RequirementKindTag {
        self.result.tag()
    }
}

/// Classified error from requirement addressing or return-path type checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum RequirementError {
    /// A result of the wrong family was offered for a requirement kind.
    #[error("requirement kind `{expected}` cannot accept a `{actual}` result")]
    ResultKindMismatch {
        /// Family expected by the requirement kind.
        expected: RequirementKindTag,
        /// Family actually carried by the offered result.
        actual: RequirementKindTag,
    },
    /// A resolution addressed a different requirement than the one checked.
    #[error("resolution for `{actual}` does not match requirement `{expected}`")]
    IdMismatch {
        /// Requirement identity being fulfilled.
        expected: RequirementId,
        /// Requirement identity carried by the resolution.
        actual: RequirementId,
    },
    /// The host did not supply a stable id for a requirement family.
    #[error("missing externally supplied requirement id for `{kind}`")]
    IdUnavailable {
        /// Requirement family whose id could not be supplied.
        kind: RequirementKindTag,
    },
    /// An interaction result failed its request-specific check.
    #[error("interaction result rejected: {0}")]
    Interaction(#[from] InteractionError),
}

#[cfg(test)]
mod tests {
    use super::{
        AgentPath, AgentSlot, AgentSpecRef, Interaction, InteractionResponse, NoRequirementIds,
        Requirement, RequirementError, RequirementId, RequirementIds, RequirementKind,
        RequirementKindTag, RequirementResolution, RequirementResult, SubagentOutput,
    };
    use crate::{
        agent::{AgentId, LlmStepMode, ToolSetRef},
        client::{ChatRequest, Response},
        conversation::ToolCallId,
        model::tool::{Tool, ToolCall, ToolResponse, ToolStatus},
    };
    use serde::{Serialize, de::DeserializeOwned};
    use serde_json::{Map, json};
    use std::{
        fmt::Debug,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn requirement_id(tail: &str) -> RequirementId {
        RequirementId::parse_str(&format!("018f0d9c-7b6a-7c12-8f31-1234567890{tail}"))
            .expect("requirement id")
    }

    fn tool_call_id() -> ToolCallId {
        "018f0d9c-7b6a-7c12-8f31-1234567890c1"
            .parse()
            .expect("tool call id")
    }

    fn agent_id() -> AgentId {
        "018f0d9c-7b6a-7c12-8f31-1234567890d1"
            .parse()
            .expect("agent id")
    }

    fn tool_set_ref() -> ToolSetRef {
        let tool_set_id = "018f0d9c-7b6a-7c12-8f31-1234567890f2"
            .parse()
            .expect("tool set id");
        ToolSetRef::new(
            tool_set_id,
            vec![Tool {
                name: "get_weather".to_owned(),
                description: "Look up weather.".to_owned(),
                input_schema: json!({ "type": "object" }),
            }],
        )
    }

    fn step_id() -> crate::agent::StepId {
        "018f0d9c-7b6a-7c12-8f31-1234567890e9"
            .parse()
            .expect("step id")
    }

    fn chat_request() -> ChatRequest {
        ChatRequest {
            model: "test-model".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            system: None,
            max_tokens: 16,
            temperature: None,
            stream: false,
            provider_extras: None,
        }
    }

    fn tool_call() -> ToolCall {
        ToolCall {
            id: "call-weather".to_owned(),
            name: "get_weather".to_owned(),
            input: json!({ "city": "Shanghai" }),
        }
    }

    fn response() -> Response {
        serde_json::from_value(json!({
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "hi" }]
            },
            "usage": { "input": 1, "output": 1 },
            "stop_reason": { "value": "end_turn", "raw": "end_turn" }
        }))
        .expect("response")
    }

    fn tool_response() -> ToolResponse {
        ToolResponse {
            tool_call_id: "call-weather".to_owned(),
            content: Vec::new(),
            status: ToolStatus::Ok,
            extra: Map::new(),
        }
    }

    fn interaction() -> Interaction {
        Interaction::question(step_id(), "proceed?".to_owned())
    }

    fn kind_of(tag: RequirementKindTag) -> RequirementKind {
        match tag {
            RequirementKindTag::Llm => RequirementKind::NeedLlm {
                request: chat_request(),
                mode: LlmStepMode::NonStreaming,
            },
            RequirementKindTag::Tool => RequirementKind::NeedTool {
                call_id: tool_call_id(),
                call: tool_call(),
            },
            RequirementKindTag::Interaction => RequirementKind::NeedInteraction {
                request: interaction(),
            },
            RequirementKindTag::Subagent => RequirementKind::NeedSubagent {
                spec_ref: AgentSpecRef(agent_id()),
                brief: interaction(),
                result_schema: None,
            },
            RequirementKindTag::Reconfig => RequirementKind::NeedReconfigRegistry {
                tool_set: tool_set_ref(),
            },
        }
    }

    fn result_of(tag: RequirementKindTag) -> RequirementResult {
        match tag {
            RequirementKindTag::Llm => RequirementResult::Llm(Ok(response())),
            RequirementKindTag::Tool => RequirementResult::Tool(Ok(tool_response())),
            RequirementKindTag::Interaction => {
                RequirementResult::Interaction(InteractionResponse::Answer("yes".to_owned()))
            }
            RequirementKindTag::Subagent => RequirementResult::Subagent(Ok(SubagentOutput {
                summary: "done".to_owned(),
            })),
            RequirementKindTag::Reconfig => RequirementResult::Reconfig(Ok(())),
        }
    }

    const ALL_TAGS: [RequirementKindTag; 5] = [
        RequirementKindTag::Llm,
        RequirementKindTag::Tool,
        RequirementKindTag::Interaction,
        RequirementKindTag::Subagent,
        RequirementKindTag::Reconfig,
    ];

    fn assert_json_round_trip<T>(value: &T)
    where
        T: Debug + PartialEq + Serialize + DeserializeOwned,
    {
        let encoded = serde_json::to_value(value).expect("serialize");
        let decoded: T = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(&decoded, value);
    }

    #[test]
    fn requirement_id_round_trips_transparently() {
        let id = requirement_id("e1");
        let encoded = serde_json::to_string(&id).expect("serialize id");
        assert_eq!(encoded, format!("\"{id}\""));
        assert_json_round_trip(&id);
    }

    #[test]
    fn agent_path_starts_at_root_and_extends() {
        let root = AgentPath::root();
        assert!(root.is_root());
        assert!(root.is_empty());
        assert_eq!(root.len(), 0);

        let child = root.child(AgentSlot::new(2)).child(AgentSlot::new(5));
        assert!(!child.is_root());
        assert_eq!(child.len(), 2);
        assert_eq!(child.slots(), &[AgentSlot::new(2), AgentSlot::new(5)][..]);
        assert_eq!(child.iter().count(), 2);

        assert_json_round_trip(&root);
        assert_json_round_trip(&child);
    }

    #[test]
    fn every_requirement_kind_round_trips() {
        for tag in ALL_TAGS {
            let requirement = Requirement::at_root(requirement_id("a1"), kind_of(tag));
            assert_eq!(requirement.tag(), tag);
            assert_json_round_trip(&requirement);
            assert_json_round_trip(&requirement.kind);
        }
    }

    #[test]
    fn requirement_preserves_non_root_origin_across_serde() {
        let origin = AgentPath::root().child(AgentSlot::new(1));
        let requirement = Requirement::new(
            requirement_id("b2"),
            origin.clone(),
            kind_of(RequirementKindTag::Tool),
        );
        let encoded = serde_json::to_value(&requirement).expect("serialize");
        let decoded: Requirement = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded.origin, origin);
        assert_eq!(decoded, requirement);
    }

    #[test]
    fn accepts_matrix_pairs_each_kind_with_its_result_only() {
        for kind_tag in ALL_TAGS {
            let kind = kind_of(kind_tag);
            for result_tag in ALL_TAGS {
                let result = result_of(result_tag);
                let outcome = kind.accepts(&result);
                if kind_tag == result_tag {
                    assert!(
                        outcome.is_ok(),
                        "kind {kind_tag} should accept {result_tag} result"
                    );
                } else {
                    assert_eq!(
                        outcome,
                        Err(RequirementError::ResultKindMismatch {
                            expected: kind_tag,
                            actual: result_tag,
                        }),
                        "kind {kind_tag} must reject {result_tag} result"
                    );
                }
            }
        }
    }

    #[test]
    fn accepts_resolution_checks_id_then_type() {
        let requirement =
            Requirement::at_root(requirement_id("c3"), kind_of(RequirementKindTag::Llm));

        let matched =
            RequirementResolution::new(requirement.id, result_of(RequirementKindTag::Llm));
        assert_eq!(requirement.accepts_resolution(&matched), Ok(()));

        let wrong_id =
            RequirementResolution::new(requirement_id("c4"), result_of(RequirementKindTag::Llm));
        assert_eq!(
            requirement.accepts_resolution(&wrong_id),
            Err(RequirementError::IdMismatch {
                expected: requirement.id,
                actual: requirement_id("c4"),
            })
        );

        let wrong_type =
            RequirementResolution::new(requirement.id, result_of(RequirementKindTag::Tool));
        assert_eq!(
            requirement.accepts_resolution(&wrong_type),
            Err(RequirementError::ResultKindMismatch {
                expected: RequirementKindTag::Llm,
                actual: RequirementKindTag::Tool,
            })
        );
    }

    #[test]
    fn no_requirement_ids_returns_classified_error_for_each_family() {
        for tag in ALL_TAGS {
            assert_eq!(
                NoRequirementIds.next_requirement_id(tag),
                Err(RequirementError::IdUnavailable { kind: tag })
            );
        }
    }

    #[test]
    fn host_supplied_ids_are_drawn_in_order_then_exhausted() {
        #[derive(Debug)]
        struct QueueIds {
            ids: Vec<RequirementId>,
            cursor: AtomicUsize,
        }

        impl RequirementIds for QueueIds {
            fn next_requirement_id(
                &self,
                kind_tag: RequirementKindTag,
            ) -> Result<RequirementId, RequirementError> {
                let index = self.cursor.fetch_add(1, Ordering::SeqCst);
                match self.ids.get(index) {
                    Some(id) => Ok(*id),
                    None => Err(RequirementError::IdUnavailable { kind: kind_tag }),
                }
            }
        }

        let supplier = QueueIds {
            ids: vec![requirement_id("f1"), requirement_id("f2")],
            cursor: AtomicUsize::new(0),
        };

        assert_eq!(
            supplier.next_requirement_id(RequirementKindTag::Llm),
            Ok(requirement_id("f1"))
        );
        assert_eq!(
            supplier.next_requirement_id(RequirementKindTag::Tool),
            Ok(requirement_id("f2"))
        );
        assert_eq!(
            supplier.next_requirement_id(RequirementKindTag::Interaction),
            Err(RequirementError::IdUnavailable {
                kind: RequirementKindTag::Interaction,
            })
        );
    }

    #[test]
    fn interaction_types_round_trip() {
        assert_json_round_trip(&interaction());
        assert_json_round_trip(&InteractionResponse::Answer("ok".to_owned()));
        assert_json_round_trip(&AgentSpecRef(agent_id()));
    }

    #[test]
    fn requirement_kind_tag_display_is_stable() {
        assert_eq!(RequirementKindTag::Llm.to_string(), "llm");
        assert_eq!(RequirementKindTag::Tool.to_string(), "tool");
        assert_eq!(RequirementKindTag::Interaction.to_string(), "interaction");
        assert_eq!(RequirementKindTag::Subagent.to_string(), "subagent");
    }
}
