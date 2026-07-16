//! Single source of truth for the effect coproduct: the effect manifest and the
//! declarative macros that expand it.
//!
//! # Why this exists
//!
//! Adding one effect to the Agent layer once meant editing eight aligned places
//! (design doc [`docs/effect-refine.md`](../../../docs/effect-refine.md) §4.1):
//! the three coproduct enums in [`requirement`](crate::agent::requirement) —
//! [`RequirementKind`](crate::agent::requirement::RequirementKind),
//! [`RequirementResult`](crate::agent::requirement::RequirementResult), and
//! [`RequirementKindTag`](crate::agent::requirement::RequirementKindTag) — plus
//! their `accepts` alignment; and the handler fan-out in
//! [`drive`](crate::agent::drive) — the
//! [`HandlerScope`](crate::agent::drive::HandlerScope) accessors, `scope_handles`,
//! and `fulfill_with_scope`. Every one of them was aligned only by convention.
//! This module collapses that fan-out into a single effect manifest so "add an
//! effect" becomes "add one manifest stanza" (see the appendix of the design doc
//! for the one-stanza diff).
//!
//! The machine-internal resume dispatch (design doc §4.1 point 8) stays
//! hand-written: it depends on a concrete machine's cursor phases and is not
//! generated here.
//!
//! # Shape: one manifest, two generators
//!
//! [`with_effect_manifest`] holds the manifest — one stanza per effect — and
//! nothing else. It is a *callback* macro: it forwards the whole manifest to a
//! generator macro named by the caller. Two generators consume it:
//!
//! - [`define_effect_coproduct`], invoked in
//!   [`requirement`](crate::agent::requirement), renders design doc §4.1 points
//!   1–4: the `RequirementKindTag` / `RequirementKind` / `RequirementResult`
//!   enums, `RequirementKindTag`'s `Display`, the `tag()` accessors, and
//!   `RequirementKind::accepts`.
//! - [`define_effect_fan_out`], invoked in [`drive`](crate::agent::drive),
//!   renders points 5–7: the `HandlerScope` accessors (each defaulting to
//!   `None`), `scope_handles`, and `fulfill_with_scope`.
//!
//! Splitting the generators keeps each fan-out point in the module design doc
//! §4.1 assigns it — the coproduct next to its serde types, the handler fan-out
//! next to the handler traits — while the manifest itself lives in exactly one
//! place. The bare type names in the manifest (`ChatRequest`, `LlmHandler`, …)
//! resolve at each generator's expansion site, so the coproduct's field/result
//! types resolve in `requirement` and the fan-out's handler traits resolve in
//! `drive`; neither module needs the other's imports.
//!
//! # Choice of `macro_rules!` over a proc-macro crate
//!
//! Design doc §4.4 allows degrading to a standalone proc-macro crate if a
//! declarative macro cannot express the manifest. It can: the split derive
//! (`RequirementKind` derives `serde`, `RequirementResult` does not),
//! struct-shaped kind variants versus single-payload tuple result variants,
//! per-field `serde` attributes (`NeedSubagent::result_schema`), boxed result
//! payloads (`ExternalSession(Box<…>)`), and the optional `needs_outer` /
//! `accepts_check` / `fulfill` markers all fit a single `macro_rules!` matcher.
//! Following the [`define_id!`](crate::agent::id) precedent, per-item rustdoc is
//! synthesized with `concat!`/`stringify!`, so the generated items satisfy
//! `#![warn(missing_docs)]` without hand-threaded doc strings. No proc-macro
//! crate is introduced.
//!
//! # Manifest grammar
//!
//! ```ignore
//! with_effect_manifest! {
//!     <TagName> {
//!         // snake_case wire/display name for this family's tag.
//!         tag_name: "<snake_case>",
//!         // The persistable request variant: its name and struct-shaped fields.
//!         // A field may carry leading attributes (e.g. `serde`), which are
//!         // forwarded verbatim onto the generated `RequirementKind` field.
//!         kind: <NeedVariant> {
//!             <field>: <FieldTy>,
//!             #[serde(default, skip_serializing_if = "Option::is_none")]
//!             <field>: <FieldTy>,
//!         },
//!         // Inner payload of the tuple-shaped result variant
//!         // (`RequirementResult::<TagName>(<ResultTy>)`).
//!         result: <ResultTy>,
//!         // Handler trait fulfilling this family and its `HandlerScope` accessor.
//!         handler: <HandlerTrait>,
//!         accessor: <accessor_fn>,
//!         // Optional (required for every non-`needs_outer` family): the exact
//!         // argument list handed to `<HandlerTrait>::fulfill`, before the
//!         // trailing `ctx`. It names the kind fields bound by the match and
//!         // encodes each field's by-value/by-reference passing (e.g.
//!         // `(request, *mode)`), which the field types alone cannot express
//!         // (design doc §4.3). `fulfill_with_scope` expands it into the call.
//!         fulfill: (<arg>, <arg>),
//!         // Optional: this family deepens the scope chain and is routed serially
//!         // through `resolve_requirement` rather than fulfilled in place, so
//!         // `fulfill_with_scope` yields `None` for it and it carries no
//!         // `fulfill` clause (only `Subagent`).
//!         needs_outer: true,
//!         // Optional: an extra `<field>.<method>(result_payload)` check `accepts`
//!         // runs after the family match, folding its error into
//!         // `RequirementError` via `?` (only `Interaction`'s
//!         // `request.accepts_response`). The named field is the check receiver;
//!         // such a family must have a single-payload check receiver.
//!         accepts_check: <field>.<method>,
//!     }
//!     // …one stanza per effect…
//! }
//! ```
//!
//! Each stanza is the whole truth about one effect. `define_effect_coproduct`
//! consumes `TagName`, `tag_name`, `kind`, `result`, and `accepts_check`; while
//! `define_effect_fan_out` consumes `TagName`, `handler`, `accessor`, `fulfill`,
//! and `needs_outer`. Both parse the full grammar and ignore the fields they do
//! not need.

/// Holds the effect manifest and forwards it to a generator macro.
///
/// The single source of truth for the effect coproduct: one stanza per effect,
/// in the grammar documented on the [module](self). Invoked as
/// `with_effect_manifest!(<generator>)`, it expands to `<generator>! { <manifest> }`,
/// so [`define_effect_coproduct`] and [`define_effect_fan_out`] each render their
/// half of the fan-out from the same manifest. Adding an effect means adding one
/// stanza here and nothing else.
macro_rules! with_effect_manifest {
    ($generator:ident) => {
        $generator! {
            Llm {
                tag_name: "llm",
                kind: NeedLlm {
                    request: ChatRequest,
                    mode: LlmStepMode,
                },
                result: Result<Response, ClientError>,
                handler: LlmHandler,
                accessor: llm,
                fulfill: (request, *mode),
            }
            Tool {
                tag_name: "tool",
                kind: NeedTool {
                    call_id: ToolCallId,
                    call: ToolCall,
                },
                result: Result<ToolResponse, ToolRuntimeError>,
                handler: ToolHandler,
                accessor: tool,
                fulfill: (*call_id, call),
            }
            Interaction {
                tag_name: "interaction",
                kind: NeedInteraction {
                    request: Interaction,
                },
                result: InteractionResponse,
                handler: InteractionHandler,
                accessor: interaction,
                fulfill: (request),
                accepts_check: request.accepts_response,
            }
            Subagent {
                tag_name: "subagent",
                kind: NeedSubagent {
                    spec_ref: AgentSpecRef,
                    brief: Interaction,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    result_schema: Option<Value>,
                },
                result: Result<SubagentOutput, AgentError>,
                handler: SubagentHandler,
                accessor: subagent,
                needs_outer: true,
            }
            Reconfig {
                tag_name: "reconfig",
                kind: NeedReconfigRegistry {
                    tool_set: ToolSetRef,
                },
                result: Result<(), ToolRuntimeError>,
                handler: ReconfigHandler,
                accessor: reconfig,
                fulfill: (tool_set),
            }
            ExternalSession {
                tag_name: "external_session",
                kind: NeedExternalSession {
                    request: ExternalSessionRequest,
                },
                result: Box<ExternalSessionResult>,
                handler: ExternalSessionHandler,
                accessor: external,
                fulfill: (request),
            }
        }
    };
}

/// Renders the effect coproduct (design doc §4.1 points 1–4) from the manifest.
///
/// Invoked as `with_effect_manifest!(define_effect_coproduct)` in
/// [`requirement`](crate::agent::requirement). It emits the `RequirementKindTag`
/// / `RequirementKind` / `RequirementResult` enums, `RequirementKindTag`'s
/// `Display`, the `tag()` accessors, and `RequirementKind::accepts`. The field
/// and result types name themselves in the manifest and resolve in the invoking
/// module. `RequirementError` (returned by `accepts`) is hand-written and stays
/// in `requirement`.
macro_rules! define_effect_coproduct {
    ($(
        $tag:ident {
            tag_name: $tag_name:literal,
            kind: $kind_variant:ident {
                $( $( #[$field_meta:meta] )* $field:ident : $field_ty:ty ),* $(,)?
            },
            result: $result_ty:ty,
            handler: $handler:ident,
            accessor: $accessor:ident,
            $( fulfill: ( $( $fulfill_arg:tt )* ), )?
            $( needs_outer: $needs_outer:literal, )?
            $( accepts_check: $accepts_recv:ident . $accepts_check:ident, )?
        }
    )+) => {
        /// Discriminant identifying which family a [`RequirementKind`] or
        /// [`RequirementResult`] belongs to.
        ///
        /// The tag drives return-path type alignment
        /// ([`RequirementKind::accepts`]) and lets a host allocate ids per
        /// requirement family via [`RequirementIds`]. Generated from the effect
        /// manifest (see the `effect_manifest` module).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum RequirementKindTag {
            $(
                #[doc = concat!("The `", stringify!($tag), "` requirement family.")]
                $tag,
            )+
        }

        impl fmt::Display for RequirementKindTag {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                let text = match self {
                    $( Self::$tag => $tag_name, )+
                };
                formatter.write_str(text)
            }
        }

        /// What a [`Requirement`] needs fulfilled. Payloads reuse existing types.
        ///
        /// The persistable *description* of a request; it derives `serde` for
        /// cross-process persistence. Generated from the effect manifest (see the
        /// `effect_manifest` module).
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum RequirementKind {
            $(
                #[doc = concat!(
                    "Reified `", stringify!($tag), "` effect requirement (`",
                    stringify!($kind_variant), "`)."
                )]
                $kind_variant {
                    $(
                        #[doc = concat!(
                            "The `", stringify!($field), "` payload of a `",
                            stringify!($kind_variant), "` requirement."
                        )]
                        $( #[$field_meta] )*
                        $field: $field_ty,
                    )*
                },
            )+
        }

        impl RequirementKind {
            /// Returns the family this kind belongs to.
            #[must_use]
            pub const fn tag(&self) -> RequirementKindTag {
                match self {
                    $( Self::$kind_variant { .. } => RequirementKindTag::$tag, )+
                }
            }

            /// Checks that `result` is type-aligned with this requirement kind.
            ///
            /// A `NeedLlm` requirement only accepts an [`RequirementResult::Llm`]
            /// result, and so on for each family. A family that declares an
            /// `accepts_check` in the manifest (only `NeedInteraction`, whose
            /// carried [`InteractionResponse`] is validated against its
            /// [`Interaction`] request) runs that extra post-validation after the
            /// family match.
            ///
            /// # Errors
            ///
            /// Returns [`RequirementError::ResultKindMismatch`] when the result
            /// family does not match this requirement's family, or the
            /// family-specific error surfaced by an `accepts_check` (e.g.
            /// [`RequirementError::Interaction`]).
            pub fn accepts(&self, result: &RequirementResult) -> Result<(), RequirementError> {
                let expected = match self {
                    $( Self::$kind_variant { .. } => RequirementKindTag::$tag, )+
                };
                let actual = match result {
                    $( RequirementResult::$tag(_) => RequirementKindTag::$tag, )+
                };
                if expected != actual {
                    return Err(RequirementError::ResultKindMismatch { expected, actual });
                }
                $(
                    // Only families with a manifest `accepts_check` emit a
                    // post-validation arm. The check is called on the named kind
                    // payload field with the result payload; `?` folds its error
                    // into `RequirementError` via that variant's `#[from]` (only
                    // `Interaction`'s `accepts_response` uses this today).
                    $(
                        if let (
                            Self::$kind_variant { $accepts_recv, .. },
                            RequirementResult::$tag(response),
                        ) = (self, result)
                        {
                            $accepts_recv.$accepts_check(response)?;
                        }
                    )?
                )+
                Ok(())
            }
        }

        /// Fulfilled result for one requirement, delivered back on the return
        /// path.
        ///
        /// The runtime half: it carries live values and runtime errors
        /// ([`ClientError`], [`ToolRuntimeError`], [`AgentError`]) and is
        /// intentionally not persistable (it does **not** derive `serde`).
        /// Generated from the effect manifest (see the `effect_manifest`
        /// module).
        #[derive(Clone, Debug)]
        pub enum RequirementResult {
            $(
                #[doc = concat!("Fulfilled result for the `", stringify!($tag), "` requirement family.")]
                $tag($result_ty),
            )+
        }

        impl RequirementResult {
            /// Returns the family this result belongs to.
            #[must_use]
            pub const fn tag(&self) -> RequirementKindTag {
                match self {
                    $( Self::$tag(_) => RequirementKindTag::$tag, )+
                }
            }
        }
    };
}

/// Renders the handler fan-out (design doc §4.1 points 5–7) from the manifest.
///
/// Invoked as `with_effect_manifest!(define_effect_fan_out)` in
/// [`drive`](crate::agent::drive). It emits the [`HandlerScope`] trait (one
/// accessor per family, each defaulting to `None`), `scope_handles`, and
/// `fulfill_with_scope`. The handler traits it borrows name themselves in the
/// manifest and resolve in the invoking module, next to their definitions.
macro_rules! define_effect_fan_out {
    ($(
        $tag:ident {
            tag_name: $tag_name:literal,
            kind: $kind_variant:ident {
                $( $( #[$field_meta:meta] )* $field:ident : $field_ty:ty ),* $(,)?
            },
            result: $result_ty:ty,
            handler: $handler:ident,
            accessor: $accessor:ident,
            $( fulfill: ( $( $fulfill_arg:tt )* ), )?
            $( needs_outer: $needs_outer:literal, )?
            $( accepts_check: $accepts_recv:ident . $accepts_check:ident, )?
        }
    )+) => {
        /// One drain layer's set of effect handlers.
        ///
        /// A scope exposes up to one handler per requirement family. Each
        /// accessor defaults to `None`, so an empty scope handles nothing and
        /// every requirement pops to the outer scope. A layer overrides only the
        /// families it can fulfill. Generated from the effect manifest (see the
        /// `effect_manifest` module); see the [module docs](self) for how scopes
        /// compose into a drain.
        pub trait HandlerScope: Send + Sync {
            $(
                #[doc = concat!(
                    "Returns this layer's [`", stringify!($handler),
                    "`], if it fulfills the `", stringify!($tag), "` family."
                )]
                fn $accessor(&self) -> Option<&dyn $handler> {
                    None
                }
            )+
        }

        /// Returns whether `scope` offers a handler for the given requirement
        /// family.
        ///
        /// Generated from the effect manifest; it must stay consistent with
        /// `fulfill_with_scope` for the same tag.
        fn scope_handles(scope: &dyn HandlerScope, tag: RequirementKindTag) -> bool {
            match tag {
                $( RequirementKindTag::$tag => scope.$accessor().is_some(), )+
            }
        }

        /// Fulfills `kind` with this scope's handler, if it has one.
        ///
        /// Returns `None` when the scope offers no handler for the family (the
        /// caller then pops it outward). Families marked `needs_outer` in the
        /// manifest (only `Subagent`) always yield `None`: they deepen the scope
        /// chain and are routed serially through `resolve_requirement` instead of
        /// being fulfilled in place. Generated from the effect manifest.
        async fn fulfill_with_scope(
            kind: &RequirementKind,
            scope: &dyn HandlerScope,
            ctx: &RunContext,
        ) -> Option<RequirementResult> {
            match kind {
                $(
                    // One arm per family. The payload fields are bound so the
                    // manifest `fulfill` arguments can name them;
                    // `#[allow(unused_variables)]` covers the `needs_outer`
                    // family (`Subagent`), whose fields go unused because its
                    // body is `None`. Exactly one of the two mutually exclusive
                    // bodies below expands: `needs_outer` families yield `None`
                    // (routed serially through `resolve_requirement`), every
                    // other family calls its handler in place.
                    #[allow(unused_variables)]
                    RequirementKind::$kind_variant { $( $field ),* } => {
                        $( let _: bool = $needs_outer; None )?
                        $( Some(scope.$accessor()?.fulfill( $( $fulfill_arg )*, ctx ).await) )?
                    }
                )+
            }
        }
    };
}

pub(crate) use {define_effect_coproduct, define_effect_fan_out, with_effect_manifest};
