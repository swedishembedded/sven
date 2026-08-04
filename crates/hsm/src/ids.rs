//! Strongly-typed newtype identifiers used throughout the kernel.
//!
//! Each ID wraps a [`uuid::Uuid`] so that two different kinds of ID can never be
//! confused at a call site (the type system makes the illegal mix-up
//! unrepresentable). All IDs are `Copy`, cheaply hashable, and `serde`
//! serializable so they can live in events, effects, audit records, and on the
//! wire.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Generates a newtype wrapper around [`Uuid`] with a uniform constructor API.
macro_rules! uuid_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub struct $name(pub Uuid);

        impl $name {
            /// Creates a fresh, random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Wraps an existing [`Uuid`].
            #[must_use]
            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            /// Returns the inner [`Uuid`].
            #[must_use]
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }

            /// Create an ID from a string: parse as UUID when the string is a
            /// valid UUID, otherwise generate a fresh random identifier.
            ///
            /// The resulting `ToolCallId` is stored by the TurnExecutor into
            /// the `call_id → thread` registry and forwarded inside
            /// [`ProposedToolCall`] so machines can reference the same ID when
            /// emitting `Effect::CallTool`.
            #[must_use]
            pub fn from_str_lossy(s: &str) -> Self {
                s.parse::<Uuid>().map(Self).unwrap_or_else(|_| Self::new())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

uuid_newtype!(
    /// Identifies a single state-machine instance (root or submachine).
    MachineId
);
uuid_newtype!(
    /// Correlates a chain of events/effects belonging to one logical request.
    CorrelationId
);
uuid_newtype!(
    /// Identifies a unit of work tracked in the machine's backlog.
    TaskId
);
uuid_newtype!(
    /// Identifies an outstanding human-approval request.
    ApprovalId
);
uuid_newtype!(
    /// Identifies a single tool invocation so its result can be matched back.
    ToolCallId
);
uuid_newtype!(
    /// Identifies a scheduled timer so its timeout/cancellation can be matched.
    TimerId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_of_different_kinds_are_distinct_types() {
        let a = ToolCallId::new();
        let b = ToolCallId::from_uuid(a.as_uuid());
        assert_eq!(a, b);
        assert_ne!(ToolCallId::new(), ToolCallId::new());
    }

    #[test]
    fn ids_round_trip_through_serde() {
        let id = ApprovalId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: ApprovalId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
