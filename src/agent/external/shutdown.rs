//! How an external session's process/connection was closed.
//!
//! Cancellation of an external agent is **never-resume** (design §6.4): once a
//! [`RunContext`](crate::agent::RunContext) is cancelled the driver abandons the
//! continuation and the machine is not stepped again, so the abandoned
//! continuation can never emit a graceful
//! [`Shutdown`](super::ExternalSessionInput::Shutdown) input. The live session
//! (CLI process, SDK connection, background reader task) is therefore closed by
//! the handle layer — [`ExternalRuntimeHandles`](super::ExternalRuntimeHandles)
//! and/or a session registry — not by the machine.
//!
//! Because a forced close may leave unrollbackable shell/edit/network side
//! effects behind, *how* the session closed must be recorded so a scheduler can
//! decide whether the worktree is safe to reuse as clean (design §6.4, §10).
//! [`ExternalSessionShutdown`] is that classification; the trace surfaces it via
//! [`TraceHandle::record_external_shutdown`](crate::agent::TraceHandle::record_external_shutdown).

use serde::{Deserialize, Serialize};

/// Disposition of closing an external session's process or connection.
///
/// This is a small, `Copy` classification meant for trace/scheduler
/// bookkeeping; the *detailed* failure text of a botched close lives in
/// [`ExternalAgentError::ShutdownFailed`](super::ExternalAgentError::ShutdownFailed),
/// not here. A close is exactly one of:
///
/// - [`Graceful`](Self::Graceful): the normal-path
///   [`Shutdown`](super::ExternalSessionInput::Shutdown) (or an equivalent
///   clean stop) completed and the session ended cleanly.
/// - [`ForcedKill`](Self::ForcedKill): the never-resume cancel path force-closed
///   the session (killed the process, dropped the connection). Side effects the
///   runtime already performed cannot be rolled back.
/// - [`Failed`](Self::Failed): closing the session did not complete cleanly, so
///   an unmanaged process or unreconciled side effect may remain. This covers
///   both a close that itself errored and a child that exited with a non-zero
///   status: the runtime signalled failure, so its partial side effects cannot
///   be trusted as clean.
///
/// [`ForcedKill`](Self::ForcedKill) and [`Failed`](Self::Failed) both signal
/// "side effects may remain", so [`leaves_residual_side_effects`] returns `true`
/// for them and a scheduler should not reuse the worktree as clean by default
/// (design §6.4, §10).
///
/// [`leaves_residual_side_effects`]: Self::leaves_residual_side_effects
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSessionShutdown {
    /// The session was closed cleanly on the normal path.
    Graceful,
    /// The session was force-closed on the never-resume cancel path.
    ForcedKill,
    /// Closing the session failed; a process or side effect may remain.
    ///
    /// Reported both when the close itself errors (wait/kill failure) and when
    /// the child exits with a non-zero status.
    Failed,
}

impl ExternalSessionShutdown {
    /// Returns `true` when the close may have left unrollbackable side effects.
    ///
    /// A [`Graceful`](Self::Graceful) close leaves none; a
    /// [`ForcedKill`](Self::ForcedKill) or a [`Failed`](Self::Failed) close both
    /// may, so a scheduler should treat the worktree as potentially dirty
    /// (design §6.4, §10).
    #[must_use]
    pub const fn leaves_residual_side_effects(self) -> bool {
        matches!(self, Self::ForcedKill | Self::Failed)
    }

    /// Returns the stable snake-case label of this disposition for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Graceful => "graceful",
            Self::ForcedKill => "forced_kill",
            Self::Failed => "failed",
        }
    }

    /// Folds two close dispositions into the more severe one.
    ///
    /// A session that closes several processes over its lifetime (Codex/OpenCode
    /// spawn one CLI process per turn) reports a single disposition at
    /// [`shutdown`](super::ExternalRuntimeSession::shutdown); this fold keeps the
    /// strongest residual signal seen along the way so a mid-session
    /// [`ForcedKill`](Self::ForcedKill) or [`Failed`](Self::Failed) close still
    /// marks the session as leaving residual side effects (review M-EXT-5).
    /// Severity order: `Graceful < Failed < ForcedKill` — a force-kill is the
    /// strongest signal of unrollbackable side effects.
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::ForcedKill, _) | (_, Self::ForcedKill) => Self::ForcedKill,
            (Self::Failed, _) | (_, Self::Failed) => Self::Failed,
            (Self::Graceful, Self::Graceful) => Self::Graceful,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ExternalSessionShutdown;
    use serde_json::json;

    #[test]
    fn residual_side_effects_only_for_forced_or_failed() {
        assert!(!ExternalSessionShutdown::Graceful.leaves_residual_side_effects());
        assert!(ExternalSessionShutdown::ForcedKill.leaves_residual_side_effects());
        assert!(ExternalSessionShutdown::Failed.leaves_residual_side_effects());
    }

    #[test]
    fn labels_are_stable_snake_case() {
        assert_eq!(ExternalSessionShutdown::Graceful.label(), "graceful");
        assert_eq!(ExternalSessionShutdown::ForcedKill.label(), "forced_kill");
        assert_eq!(ExternalSessionShutdown::Failed.label(), "failed");
    }

    #[test]
    fn merge_keeps_the_more_severe_disposition() {
        use ExternalSessionShutdown::{Failed, ForcedKill, Graceful};
        for (left, right, expected) in [
            (Graceful, Graceful, Graceful),
            (Graceful, Failed, Failed),
            (Failed, Graceful, Failed),
            (Graceful, ForcedKill, ForcedKill),
            (ForcedKill, Graceful, ForcedKill),
            (Failed, ForcedKill, ForcedKill),
            (ForcedKill, Failed, ForcedKill),
            (Failed, Failed, Failed),
            (ForcedKill, ForcedKill, ForcedKill),
        ] {
            assert_eq!(left.merge(right), expected, "{left:?} merge {right:?}");
        }
    }

    #[test]
    fn serializes_snake_case_and_round_trips() {
        for (value, wire) in [
            (ExternalSessionShutdown::Graceful, "graceful"),
            (ExternalSessionShutdown::ForcedKill, "forced_kill"),
            (ExternalSessionShutdown::Failed, "failed"),
        ] {
            let encoded = serde_json::to_value(value).expect("serialize");
            assert_eq!(encoded, json!(wire));
            let decoded: ExternalSessionShutdown =
                serde_json::from_value(encoded).expect("deserialize");
            assert_eq!(decoded, value);
        }
    }
}
