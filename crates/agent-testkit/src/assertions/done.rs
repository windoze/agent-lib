//! Read-only assertions over a drained turn's terminal [`TurnDone`].
//!
//! [`assert_done`] asserts the turn came to rest on [`LoopCursorKind::Done`] and
//! returns a builder for further checks (terminal cursor kind, notification
//! count) and for handing the notification stream to
//! [`assert_notifications`](super::assert_notifications).

use agent_lib::agent::{LoopCursorKind, Notification, TurnDone};

/// Asserts `done` came to rest on [`LoopCursorKind::Done`] and returns a builder.
///
/// This is the common "the turn finished cleanly" check used right after a
/// [`DrainHarness`](crate::harness::DrainHarness) run; chain further assertions
/// (for example [`notification_count`](TurnDoneAssertions::notification_count))
/// off the returned builder.
pub fn assert_done(done: &TurnDone) -> TurnDoneAssertions<'_> {
    TurnDoneAssertions { done }.cursor_kind(LoopCursorKind::Done)
}

/// A fluent, read-only assertion builder over a [`TurnDone`].
#[derive(Clone, Copy)]
pub struct TurnDoneAssertions<'a> {
    done: &'a TurnDone,
}

impl<'a> TurnDoneAssertions<'a> {
    /// Returns the notifications produced over the drain, for further assertions.
    pub fn notifications(self) -> &'a [Notification] {
        self.done.notifications()
    }

    /// Asserts the terminal cursor kind the turn came to rest on.
    pub fn cursor_kind(self, expected: LoopCursorKind) -> Self {
        let actual = self.done.cursor().kind();
        assert!(
            actual == expected,
            "expected the turn to end on {expected:?}, found {actual:?} (with {} notification(s))",
            self.done.notifications().len()
        );
        self
    }

    /// Asserts the turn ended on [`LoopCursorKind::Error`].
    pub fn errored(self) -> Self {
        self.cursor_kind(LoopCursorKind::Error)
    }

    /// Asserts the number of notifications produced over the drain.
    pub fn notification_count(self, expected: usize) -> Self {
        let actual = self.done.notifications().len();
        assert!(
            actual == expected,
            "expected {expected} notification(s), found {actual}"
        );
        self
    }
}

#[cfg(test)]
mod tests {
    use super::assert_done;
    use crate::fixtures::{
        agent_spec_with_tools, agent_state, assistant_text, default_machine, root_context, usage,
        user_input,
    };
    use crate::handlers::ScriptedLlmHandler;
    use crate::ids::SeqIds;
    use crate::scope::TestScope;
    use crate::script::LlmStep;
    use agent_lib::agent::{LoopCursorKind, drain};
    use std::sync::Arc;

    /// Drains a text-only turn to completion and returns its `TurnDone`.
    async fn text_turn_done() -> agent_lib::agent::TurnDone {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let spec = agent_spec_with_tools(&ids, vec![]);
        let mut machine = default_machine(&ids, agent_state(&ids, spec));
        let llm =
            ScriptedLlmHandler::from_steps([LlmStep::response(assistant_text("hi", usage(3, 2)))]);
        let scope = TestScope::builder().llm(Arc::new(llm)).build();
        drain(&mut machine, user_input(&ids, "hello"), &scope, None, &ctx)
            .await
            .expect("text turn drains")
    }

    #[tokio::test]
    async fn happy_path_asserts_done_cursor() {
        let done = text_turn_done().await;
        assert_done(&done).cursor_kind(LoopCursorKind::Done);
    }

    #[tokio::test]
    async fn wrong_cursor_failure_message_reports_actual() {
        let done = text_turn_done().await;
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_done(&done).errored();
        }))
        .expect_err("asserting Error on a Done turn must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("expected the turn to end on Error, found Done"),
            "message names expected and actual: {message}"
        );
    }
}
