//! Strongly typed identities used by Conversation data.
//!
//! Every identity wraps an externally supplied UUID. This module deliberately
//! exposes no random, clock-based, or database-backed generation path. In
//! production, callers should normally supply UUIDv7 values (or another
//! globally stable equivalent) so replay and deterministic tests stay under
//! caller control.
//!
//! The wrappers are nominally distinct and therefore cannot be mixed at API
//! boundaries:
//!
//! ```compile_fail
//! use agent_lib::conversation::id::{ConversationId, TurnId};
//!
//! let conversation_id: ConversationId =
//!     "018f0d9c-7b6a-7c12-8f31-1234567890ab".parse().unwrap();
//! let _turn_id: TurnId = conversation_id;
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
    /// Identifies one Conversation, including a branch created by a future fork.
    ConversationId
);

define_id!(
    /// Identifies one complete exchange cycle and its stable history boundary.
    TurnId
);

define_id!(
    /// Identifies one immutable Conversation message as a globally stable key.
    MessageId
);

define_id!(
    /// Identifies framework-level tool-call bookkeeping independently of provider ids.
    ToolCallId
);

define_id!(
    /// Identifies a future non-destructive projection artifact.
    ArtifactId
);

#[cfg(test)]
mod tests {
    use super::{ArtifactId, ConversationId, MessageId, ToolCallId, TurnId};
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
    fn every_identity_has_a_canonical_uuid_serde_shape() {
        assert_json_round_trip(
            ConversationId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890ab")
                .expect("conversation id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890ab",
        );
        assert_json_round_trip(
            TurnId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890ac").expect("turn id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890ac",
        );
        assert_json_round_trip(
            MessageId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890ad").expect("message id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890ad",
        );
        assert_json_round_trip(
            ToolCallId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890ae").expect("tool call id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890ae",
        );
        assert_json_round_trip(
            ArtifactId::parse_str("018f0d9c-7b6a-7c12-8f31-1234567890af").expect("artifact id"),
            "018f0d9c-7b6a-7c12-8f31-1234567890af",
        );
    }

    #[test]
    fn constructors_preserve_the_exact_external_value() {
        let supplied = Uuid::parse_str("018f0d9c-7b6a-7c12-8f31-fedcba098765")
            .expect("externally supplied uuid");
        let id = MessageId::new(supplied);

        assert_eq!(id.as_uuid(), &supplied);
        assert_eq!(id.to_string(), supplied.to_string());
        assert_eq!(id.into_uuid(), supplied);
    }

    #[test]
    fn malformed_external_values_are_rejected() {
        let error = "generated-for-me"
            .parse::<ConversationId>()
            .expect_err("invalid UUID must not acquire an identity");

        assert!(!error.to_string().is_empty());
    }
}
