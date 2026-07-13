//! Cancellation token propagation for Agent run contexts.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Cancellation token that can be queried and derived for child work.
///
/// A child token observes its own cancellation flag and every ancestor flag.
/// Cancelling a child does not cancel the parent; cancelling the parent is
/// visible to all descendants.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    parent: Option<CancellationToken>,
}

impl CancellationToken {
    /// Creates an uncancelled root token.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                parent: None,
            }),
        }
    }

    /// Creates a child token that observes this token as its parent.
    #[must_use]
    pub fn derive_child(&self) -> Self {
        Self {
            inner: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                parent: Some(self.clone()),
            }),
        }
    }

    /// Marks this token as cancelled.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
    }

    /// Returns true when this token or any parent token has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
            || self
                .inner
                .parent
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}
