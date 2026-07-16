//! Single source of truth for the effect coproduct: the [`define_effects!`]
//! declarative macro.
//!
//! # Why this exists
//!
//! Adding one effect to the Agent layer today means editing eight aligned
//! places (design doc [`docs/effect-refine.md`](../../../docs/effect-refine.md)
//! §4.1). Three of those are the coproduct enums in
//! [`requirement`](crate::agent::requirement) —
//! [`RequirementKind`](crate::agent::requirement::RequirementKind),
//! [`RequirementResult`](crate::agent::requirement::RequirementResult), and
//! [`RequirementKindTag`](crate::agent::requirement::RequirementKindTag) — plus
//! their `accepts` alignment; the other four are the handler fan-out in
//! [`drive`](crate::agent::drive). Every one of the eight is aligned only by
//! convention. `define_effects!` collapses that fan-out into a single effect
//! manifest so "add an effect" becomes "add one manifest stanza".
//!
//! This module lands the macro (刀 (A) milestones M3-1 and M3-2). It generates
//! the first seven fan-out points from the manifest — the three coproduct enums,
//! [`RequirementKindTag`]'s `Display`, the `tag()` accessors,
//! `RequirementKind::accepts`, the [`HandlerScope`](crate::agent::drive)
//! accessors, `scope_handles`, and `fulfill_with_scope` — under the transitional
//! names `RequirementKindGen` / `RequirementResultGen` / `RequirementKindTagGen`
//! / `HandlerScopeGen` / `scope_handles_gen` / `fulfill_with_scope_gen`, which
//! **coexist** with the hand-written definitions rather than replace them. M3-2
//! adds `#[test]` assertions proving the generated products byte-for-byte and
//! behaviour equivalent to the hand-written ones; M3-3 deletes the hand-written
//! definitions and promotes the macro products to the canonical names.
//!
//! # Choice of `macro_rules!` over a proc-macro crate
//!
//! Design doc §4.4 allows degrading to a standalone proc-macro crate if a
//! declarative macro cannot express the manifest. It can: the split derive
//! (`RequirementKind` derives `serde`, `RequirementResult` does not),
//! struct-shaped kind variants versus single-payload tuple result variants,
//! per-field `serde` attributes (`NeedSubagent::result_schema`), boxed result
//! payloads (`ExternalSession(Box<…>)`), and the optional `needs_outer` /
//! `accepts_check` markers all fit a single `macro_rules!` matcher. Following
//! the [`define_id!`](crate::agent::id) precedent, per-item rustdoc is
//! synthesized with `concat!`/`stringify!`, so the generated items satisfy
//! `#![warn(missing_docs)]` without hand-threaded doc strings. No proc-macro
//! crate is introduced.
//!
//! # Manifest grammar
//!
//! ```ignore
//! define_effects! {
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
//!         // Handler trait fulfilling this family and its `HandlerScope` accessor
//!         // (consumed by M3-2's `drive` fan-out; captured but unused in M3-1).
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
//! Each stanza is the whole truth about one effect. The macro body consumes
//! `TagName`, `tag_name`, `kind`, and `result` for the coproduct enums (M3-1),
//! and `handler`, `accessor`, `fulfill`, `needs_outer`, and `accepts_check` for
//! `accepts` and the `drive` fan-out (M3-2). Later milestones only grow the
//! macro body and promote the generated names, never re-shape the manifest.

/// Generates the effect coproduct and its handler fan-out from a single effect
/// manifest. See the [module docs](self) for the grammar and staging.
///
/// It emits the transitional `RequirementKindGen` / `RequirementResultGen` /
/// `RequirementKindTagGen` types (fan-out points 1–3 of design doc §4.1) plus
/// [`RequirementKindTagGen`]'s `Display`, the `tag()` accessors, and (point 4)
/// `RequirementKindGen::accepts`, then (points 5–7) the `HandlerScopeGen`
/// accessors, `scope_handles_gen`, and `fulfill_with_scope_gen`. The machine
/// resume dispatch (point 8) stays hand-written.
macro_rules! define_effects {
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
        /// Family discriminant generated from the effect manifest.
        ///
        /// Transitional twin of
        /// [`RequirementKindTag`](crate::agent::requirement::RequirementKindTag)
        /// while the hand-written and generated coproducts coexist (milestone
        /// M3-1); see the `effect_manifest` module for the generating macro.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum RequirementKindTagGen {
            $(
                #[doc = concat!("The `", stringify!($tag), "` requirement family.")]
                $tag,
            )+
        }

        impl fmt::Display for RequirementKindTagGen {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                let text = match self {
                    $( Self::$tag => $tag_name, )+
                };
                formatter.write_str(text)
            }
        }

        /// Persistable requirement description generated from the effect
        /// manifest.
        ///
        /// Transitional twin of
        /// [`RequirementKind`](crate::agent::requirement::RequirementKind); it
        /// derives `serde` with the same `snake_case` renaming so its wire form
        /// matches variant-for-variant (milestone M3-1).
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum RequirementKindGen {
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

        impl RequirementKindGen {
            /// Returns the family this kind belongs to.
            #[must_use]
            pub const fn tag(&self) -> RequirementKindTagGen {
                match self {
                    $( Self::$kind_variant { .. } => RequirementKindTagGen::$tag, )+
                }
            }

            /// Checks that `result` is type-aligned with this requirement kind.
            ///
            /// Generated twin of
            /// [`RequirementKind::accepts`](crate::agent::requirement::RequirementKind::accepts):
            /// it first rejects a cross-family result with
            /// [`RequirementError::ResultKindMismatch`], then runs any manifest
            /// `accepts_check` post-validation (only `Interaction`'s
            /// `accepts_response`). The family discriminants it reports reuse the
            /// hand-written [`RequirementKindTag`](crate::agent::requirement::RequirementKindTag)
            /// so the returned [`RequirementError`](crate::agent::requirement::RequirementError)
            /// is value-equal to the hand-written one while both coexist
            /// (milestone M3-2).
            ///
            /// # Errors
            ///
            /// Returns [`RequirementError::ResultKindMismatch`] when the result
            /// family differs, or the family-specific error surfaced by an
            /// `accepts_check` (e.g. [`RequirementError::Interaction`]).
            pub fn accepts(&self, result: &RequirementResultGen) -> Result<(), RequirementError> {
                let expected = match self {
                    $( Self::$kind_variant { .. } => RequirementKindTag::$tag, )+
                };
                let actual = match result {
                    $( RequirementResultGen::$tag(_) => RequirementKindTag::$tag, )+
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
                            RequirementResultGen::$tag(response),
                        ) = (self, result)
                        {
                            $accepts_recv.$accepts_check(response)?;
                        }
                    )?
                )+
                Ok(())
            }
        }

        /// Runtime requirement result generated from the effect manifest.
        ///
        /// Transitional twin of
        /// [`RequirementResult`](crate::agent::requirement::RequirementResult);
        /// like the hand-written half it carries live values and runtime errors
        /// and therefore deliberately does **not** derive `serde` (milestone
        /// M3-1).
        #[derive(Clone, Debug)]
        pub enum RequirementResultGen {
            $(
                #[doc = concat!("Fulfilled result for the `", stringify!($tag), "` requirement family.")]
                $tag($result_ty),
            )+
        }

        impl RequirementResultGen {
            /// Returns the family this result belongs to.
            #[must_use]
            pub const fn tag(&self) -> RequirementKindTagGen {
                match self {
                    $( Self::$tag(_) => RequirementKindTagGen::$tag, )+
                }
            }
        }

        /// One drain layer's effect handlers, generated from the effect
        /// manifest.
        ///
        /// Transitional twin of
        /// [`HandlerScope`](crate::agent::drive::HandlerScope): it exposes one
        /// accessor per family, each defaulting to `None`, so an empty scope
        /// handles nothing and every requirement pops to the outer scope
        /// (milestone M3-2). Each accessor borrows the same hand-written handler
        /// trait the manifest names.
        pub trait HandlerScopeGen: Send + Sync {
            $(
                #[doc = concat!(
                    "Returns this layer's `", stringify!($handler),
                    "`, if it fulfills the `", stringify!($tag), "` family."
                )]
                fn $accessor(&self) -> Option<&dyn crate::agent::drive::$handler> {
                    None
                }
            )+
        }

        /// Returns whether `scope` offers a handler for the given requirement
        /// family.
        ///
        /// Generated twin of `drive::scope_handles` (milestone M3-2); it must
        /// stay consistent with [`fulfill_with_scope_gen`] for the same tag.
        #[must_use]
        pub fn scope_handles_gen(
            scope: &dyn HandlerScopeGen,
            tag: RequirementKindTagGen,
        ) -> bool {
            match tag {
                $( RequirementKindTagGen::$tag => scope.$accessor().is_some(), )+
            }
        }

        /// Fulfills `kind` with this scope's handler, if it has one.
        ///
        /// Generated twin of `drive::fulfill_with_scope` (milestone M3-2).
        /// Returns `None` when the scope offers no handler for the family (the
        /// caller then pops it outward). Families marked `needs_outer` in the
        /// manifest (only `Subagent`) always yield `None`: they deepen the scope
        /// chain and are routed serially through `resolve_requirement` instead of
        /// being fulfilled in place.
        pub async fn fulfill_with_scope_gen(
            kind: &RequirementKindGen,
            scope: &dyn HandlerScopeGen,
            ctx: &RunContext,
        ) -> Option<RequirementResult> {
            match kind {
                $(
                    // One arm per family. The payload fields are bound at this
                    // (effect) level so the manifest `fulfill` arguments can name
                    // them; `#[allow(unused_variables)]` covers the `needs_outer`
                    // family (`Subagent`), whose fields go unused because its body
                    // is `None`. Exactly one of the two mutually exclusive bodies
                    // below expands: `needs_outer` families yield `None` (they are
                    // routed serially through `resolve_requirement`), every other
                    // family calls its handler in place.
                    #[allow(unused_variables)]
                    RequirementKindGen::$kind_variant { $( $field ),* } => {
                        $( let _: bool = $needs_outer; None )?
                        $( Some(scope.$accessor()?.fulfill( $( $fulfill_arg )*, ctx ).await) )?
                    }
                )+
            }
        }
    };
}

pub(crate) use define_effects;
