//! Cancellation token propagation for Agent run contexts.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;

/// Cancellation token that can be queried and derived for child work.
///
/// A child token observes its own cancellation flag and every ancestor flag.
/// Cancelling a child does not cancel the parent; cancelling the parent is
/// visible to all descendants.
///
/// Beyond the synchronous [`is_cancelled`](Self::is_cancelled) poll, the token
/// offers an asynchronous wait point, [`cancelled`](Self::cancelled), so an
/// in-flight operation (for example an LLM request a reference handler is
/// awaiting) can race the cancel signal instead of running to completion
/// first (M4-5 / M-ERR-2: cancellation latency is bounded).
#[derive(Clone, Debug)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
    parent: Option<CancellationToken>,
}

impl CancellationToken {
    /// Creates an uncancelled root token.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
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
                notify: Notify::new(),
                parent: Some(self.clone()),
            }),
        }
    }

    /// Marks this token as cancelled.
    ///
    /// Wakes every task currently parked in [`cancelled`](Self::cancelled) on
    /// this token; descendants observe the cancellation through their parent
    /// chain (their own waiters race the ancestor notifies).
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
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

    /// Resolves as soon as this token or any ancestor token is cancelled.
    ///
    /// Returns immediately when the chain is already cancelled. The wait
    /// registers on every ancestor's [`Notify`] before re-checking the flags,
    /// so a cancel landing between the check and the park is never missed.
    pub async fn cancelled(&self) {
        // Collect the ancestor chain (self included): a wait must observe a
        // cancel landing on *any* link, exactly like `is_cancelled` does.
        let mut chain = Vec::new();
        let mut current = Some(self.clone());
        while let Some(token) = current {
            current = token.inner.parent.clone();
            chain.push(token);
        }
        let mut waiters: Vec<_> = chain
            .iter()
            .map(|token| Box::pin(token.inner.notify.notified()))
            .collect();
        // Register every waiter *before* re-checking the flags: a cancel that
        // lands after this point notifies a registered waiter, closing the
        // check-then-park race.
        for waiter in &mut waiters {
            waiter.as_mut().enable();
        }
        if self.is_cancelled() {
            return;
        }
        futures::future::select_all(waiters).await;
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::CancellationToken;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelled_returns_immediately_on_a_pre_cancelled_token() {
        let token = CancellationToken::new();
        token.cancel();

        tokio::time::timeout(Duration::from_secs(1), token.cancelled())
            .await
            .expect("a pre-cancelled token must not park");
    }

    #[tokio::test]
    async fn cancelled_wakes_when_the_token_is_cancelled_mid_wait() {
        let token = CancellationToken::new();
        let canceller = token.clone();

        let wake = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            canceller.cancel();
        });
        tokio::time::timeout(Duration::from_secs(5), token.cancelled())
            .await
            .expect("a mid-wait cancel must wake the waiter");
        wake.await.expect("canceller task");
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn cancelled_wakes_when_an_ancestor_is_cancelled_mid_wait() {
        let parent = CancellationToken::new();
        let child = parent.derive_child();
        let grandchild = child.derive_child();

        let wake = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            parent.cancel();
        });
        tokio::time::timeout(Duration::from_secs(5), grandchild.cancelled())
            .await
            .expect("an ancestor cancel must wake the descendant waiter");
        wake.await.expect("canceller task");
        assert!(grandchild.is_cancelled());
        // Cancelling the parent must not mark intermediate children themselves.
        assert!(!child.inner_flag());
    }

    #[tokio::test]
    async fn cancelled_does_not_fire_on_a_sibling_cancel() {
        let parent = CancellationToken::new();
        let sibling = parent.derive_child();
        let child = parent.derive_child();

        sibling.cancel();
        let parked = tokio::time::timeout(Duration::from_millis(50), child.cancelled()).await;
        assert!(parked.is_err(), "a sibling cancel must not wake this token");
    }

    impl CancellationToken {
        /// Test-only view of this token's own flag (no parent chain).
        fn inner_flag(&self) -> bool {
            self.inner.cancelled.load(Ordering::SeqCst)
        }
    }
}
