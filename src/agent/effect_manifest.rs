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
//! This module lands the macro **skeleton** (刀 (A) milestone M3-1). It
//! generates only the first three fan-out points from the manifest — the three
//! coproduct enums, [`RequirementKindTag`]'s `Display`, and the `tag()`
//! accessor on the kind and result enums — under the transitional names
//! `RequirementKindGen` / `RequirementResultGen` / `RequirementKindTagGen`,
//! which **coexist** with the hand-written definitions rather than replace them.
//! M3-2 extends the macro body to cover `accepts` and the `drive` fan-out and
//! proves the generated products byte-for-byte equivalent to the hand-written
//! ones; M3-3 deletes the hand-written definitions and promotes the macro
//! products to the canonical names.
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
//!         // Optional: this family deepens the scope chain and is routed serially
//!         // through `resolve_requirement` rather than fulfilled in place, so
//!         // `fulfill_with_scope` yields `None` for it (only `Subagent`).
//!         needs_outer: true,
//!         // Optional: an extra request/response check `accepts` runs after the
//!         // family match (only `Interaction`'s `accepts_response`).
//!         accepts_check: <method>,
//!     }
//!     // …one stanza per effect…
//! }
//! ```
//!
//! Each stanza is the whole truth about one effect. M3-1's macro body consumes
//! `TagName`, `tag_name`, `kind`, and `result`; `handler`, `accessor`,
//! `needs_outer`, and `accepts_check` are matched now so the manifest is
//! complete and later milestones only grow the macro body, never the manifest.

/// Generates the effect coproduct enums and their tag accessors from a single
/// effect manifest. See the [module docs](self) for the grammar and staging.
///
/// M3-1 emits the transitional `RequirementKindGen` / `RequirementResultGen` /
/// `RequirementKindTagGen` types (fan-out points 1–3 of design doc §4.1) plus
/// [`RequirementKindTagGen`]'s `Display` and the `tag()` accessor on the kind
/// and result enums. The remaining manifest fields are matched but not yet
/// expanded.
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
            $( needs_outer: $needs_outer:literal, )?
            $( accepts_check: $accepts_check:ident, )?
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
    };
}

pub(crate) use define_effects;
