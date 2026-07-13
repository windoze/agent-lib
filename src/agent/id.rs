//! Strongly typed identities used by Agent-layer data.
//!
//! Every identity wraps an externally supplied UUID. The Agent layer exposes no
//! random, clock-based, or database-backed generation path, so callers keep
//! replay, restore, and deterministic tests under their own control.
//!
//! The wrappers are nominally distinct and therefore cannot be mixed at API
//! boundaries:
//!
//! ```compile_fail
//! use agent_lib::agent::id::{AgentId, RunId};
//!
//! let agent_id: AgentId =
//!     "018f0d9c-7b6a-7c12-8f31-1234567890aa".parse().unwrap();
//! let _run_id: RunId = agent_id;
//! ```

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        #[repr(transparent)]
        pub struct $name(Uuid);

        impl $name {
            #[doc = concat!(
                "Creates a `",
                stringify!($name),
                "` from an externally supplied UUID."
            )]
            #[must_use]
            pub const fn new(value: Uuid) -> Self {
                Self(value)
            }

            #[doc = concat!(
                "Parses an externally supplied UUID into a `",
                stringify!($name),
                "`."
            )]
            ///
            /// # Errors
            ///
            /// Returns [`uuid::Error`] when `value` is not a UUID accepted by
            /// the `uuid` parser.
            pub fn parse_str(value: &str) -> Result<Self, uuid::Error> {
                Uuid::parse_str(value).map(Self::new)
            }

            #[doc = concat!(
                "Returns the externally supplied UUID inside this `",
                stringify!($name),
                "`."
            )]
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            #[doc = concat!(
                "Consumes this `",
                stringify!($name),
                "` and returns its UUID."
            )]
            #[must_use]
            pub const fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse_str(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, formatter)
            }
        }
    };
}

define_id!(
    /// Identifies one Agent static specification and its runtime state lineage.
    AgentId
);

define_id!(
    /// Identifies one externally initiated Agent run.
    RunId
);

define_id!(
    /// Identifies one Agent loop step within a run.
    StepId
);

define_id!(
    /// Identifies a declared set of tools available to an Agent.
    ToolSetId
);

define_id!(
    /// Identifies one skill bundle or activation record.
    SkillId
);

define_id!(
    /// Identifies one plan board.
    PlanId
);

define_id!(
    /// Identifies one blackboard message stream.
    BlackboardId
);

#[cfg(test)]
mod tests {
    use super::{AgentId, BlackboardId, PlanId, RunId, SkillId, StepId, ToolSetId};
    use serde::{Serialize, de::DeserializeOwned};
    use std::fmt::Debug;
    use uuid::Uuid;

    fn assert_json_round_trip<T>(value: T, expected_uuid: &str)
    where
        T: Debug + Eq + Serialize + DeserializeOwned,
    {
        let encoded = serde_json::to_string(&value).expect("serialize typed id");
        assert_eq!(encoded, format!("\"{expected_uuid}\""));

        let decoded: T = serde_json::from_str(&encoded).expect("deserialize typed id");
        assert_eq!(decoded, value);
    }

    #[test]
    fn every_agent_identity_has_a_canonical_uuid_serde_shape() {
        assert_json_round_trip(
            AgentId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a1").expect("agent id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890a1",
        );
        assert_json_round_trip(
            RunId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a2").expect("run id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890a2",
        );
        assert_json_round_trip(
            StepId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a3").expect("step id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890a3",
        );
        assert_json_round_trip(
            ToolSetId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a4").expect("tool set id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890a4",
        );
        assert_json_round_trip(
            SkillId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a5").expect("skill id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890a5",
        );
        assert_json_round_trip(
            PlanId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a6").expect("plan id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890a6",
        );
        assert_json_round_trip(
            BlackboardId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890a7").expect("blackboard id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890a7",
        );
    }

    #[test]
    fn constructors_preserve_the_exact_external_value() {
        let supplied = Uuid::parse_str("018f0d9c-7b6a-7c12-8f31-fedcba098765")
            .expect("externally supplied uuid");
        let id = AgentId::new(supplied);

        assert_eq!(id.as_uuid(), &supplied);
        assert_eq!(id.to_string(), supplied.to_string());
        assert_eq!(id.into_uuid(), supplied);
    }

    #[test]
    fn malformed_external_values_are_rejected() {
        let error = "generated-for-me"
            .parse::<AgentId>()
            .expect_err("invalid UUID must not acquire an identity");

        assert!(!error.to_string().is_empty());
    }
}
