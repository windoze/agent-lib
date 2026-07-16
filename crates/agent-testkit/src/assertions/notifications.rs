//! Read-only assertions over a [`Notification`] stream.
//!
//! [`assert_notifications`] wraps the `&[Notification]` a drain or step produced
//! (for example [`TurnDone::notifications`](agent_lib::agent::TurnDone::notifications))
//! and lets a test assert on family counts, tool start/finish presence and
//! ordering, step-boundary counts, and step-boundary metadata.

use agent_lib::agent::{ExternalAgentEvent, Notification, StepId};
use agent_lib::conversation::ToolCallId;
use serde_json::Value;

/// Starts a fluent, read-only assertion over a notification stream.
#[must_use]
pub fn assert_notifications(notifications: &[Notification]) -> NotificationAssertions<'_> {
    NotificationAssertions { notifications }
}

/// A fluent, read-only assertion builder over a slice of [`Notification`]s.
#[derive(Clone, Copy)]
pub struct NotificationAssertions<'a> {
    notifications: &'a [Notification],
}

impl<'a> NotificationAssertions<'a> {
    /// Asserts the total number of notifications in the stream.
    pub fn count(self, expected: usize) -> Self {
        let actual = self.notifications.len();
        assert!(
            actual == expected,
            "expected {expected} notification(s), found {actual}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts the number of `Llm` stream notifications.
    pub fn llm_count(self, expected: usize) -> Self {
        self.family_count(
            "llm",
            expected,
            self.notifications
                .iter()
                .filter(|n| matches!(n, Notification::Llm(_)))
                .count(),
        )
    }

    /// Asserts the number of step-boundary notifications.
    pub fn step_boundary_count(self, expected: usize) -> Self {
        self.family_count("step boundary", expected, self.step_boundary_steps().len())
    }

    /// Asserts the number of tool-started notifications.
    pub fn tool_started_count(self, expected: usize) -> Self {
        self.family_count("tool started", expected, self.tool_started_calls().len())
    }

    /// Asserts the number of tool-finished notifications.
    pub fn tool_finished_count(self, expected: usize) -> Self {
        self.family_count("tool finished", expected, self.tool_finished_calls().len())
    }

    /// Asserts the number of external-agent notifications.
    ///
    /// External-agent notifications carry the observe-only
    /// [`ExternalAgentEvent`]s an external session buffered before a decision
    /// point and replayed on resume (design §5.5); this counts them as a family,
    /// mirroring [`llm_count`](Self::llm_count) and the tool families.
    pub fn external_agent_count(self, expected: usize) -> Self {
        self.family_count(
            "external agent",
            expected,
            self.external_agent_events().len(),
        )
    }

    /// Asserts a tool-started notification exists for `call_id`.
    pub fn tool_started(self, call_id: ToolCallId) -> Self {
        assert!(
            self.tool_started_calls().contains(&call_id),
            "expected a tool-started notification for call {call_id}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts a tool-finished notification exists for `call_id`.
    pub fn tool_finished(self, call_id: ToolCallId) -> Self {
        assert!(
            self.tool_finished_calls().contains(&call_id),
            "expected a tool-finished notification for call {call_id}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts that `call_id`'s tool-started notification precedes its
    /// tool-finished notification in stream order.
    pub fn started_then_finished(self, call_id: ToolCallId) -> Self {
        let started = self.notifications.iter().position(
            |n| matches!(n, Notification::ToolCallStarted(started) if started.call_id() == call_id),
        );
        let finished = self.notifications.iter().position(|n| {
            matches!(n, Notification::ToolCallFinished(finished) if finished.call_id() == call_id)
        });
        match (started, finished) {
            (Some(start), Some(finish)) => assert!(
                start < finish,
                "expected tool started (at {start}) before finished (at {finish}) for call {call_id}\n{}",
                self.summary()
            ),
            (None, _) => panic!(
                "expected a tool-started notification for call {call_id}\n{}",
                self.summary()
            ),
            (_, None) => panic!(
                "expected a tool-finished notification for call {call_id}\n{}",
                self.summary()
            ),
        }
        self
    }

    /// Asserts a step boundary for `step_id` carries `key == expected` metadata.
    pub fn boundary_metadata_eq(self, step_id: StepId, key: &str, expected: &Value) -> Self {
        match self.boundary_metadata(step_id, key) {
            Some(value) if value == expected => self,
            Some(value) => panic!(
                "expected step {step_id} metadata {key:?} = {expected}, found {value}\n{}",
                self.summary()
            ),
            None => panic!(
                "expected step {step_id} to carry metadata {key:?}, but it was absent\n{}",
                self.summary()
            ),
        }
    }

    /// Returns the metadata value stored under `key` for step `step_id`, if the
    /// stream carries a matching step boundary that defines it.
    pub fn boundary_metadata(self, step_id: StepId, key: &str) -> Option<&'a Value> {
        self.notifications
            .iter()
            .find_map(|notification| match notification {
                Notification::StepBoundary(boundary) if boundary.step_id() == step_id => {
                    boundary.metadata().get(key)
                }
                _ => None,
            })
    }

    /// Returns the call ids of every tool-started notification, in stream order.
    pub fn tool_started_calls(self) -> Vec<ToolCallId> {
        self.notifications
            .iter()
            .filter_map(|notification| match notification {
                Notification::ToolCallStarted(started) => Some(started.call_id()),
                _ => None,
            })
            .collect()
    }

    /// Returns the call ids of every tool-finished notification, in stream order.
    pub fn tool_finished_calls(self) -> Vec<ToolCallId> {
        self.notifications
            .iter()
            .filter_map(|notification| match notification {
                Notification::ToolCallFinished(finished) => Some(finished.call_id()),
                _ => None,
            })
            .collect()
    }

    /// Returns the step ids of every step-boundary notification, in stream order.
    pub fn step_boundary_steps(self) -> Vec<StepId> {
        self.notifications
            .iter()
            .filter_map(|notification| match notification {
                Notification::StepBoundary(boundary) => Some(boundary.step_id()),
                _ => None,
            })
            .collect()
    }

    /// Returns every external-agent event in the stream, in stream order.
    ///
    /// These are the observe-only [`ExternalAgentEvent`]s replayed from an
    /// external session's buffered `observations` (design §5.5), letting a test
    /// assert on their payloads and ordering.
    pub fn external_agent_events(self) -> Vec<&'a ExternalAgentEvent> {
        self.notifications
            .iter()
            .filter_map(|notification| match notification {
                Notification::ExternalAgent(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    fn family_count(self, label: &str, expected: usize, actual: usize) -> Self {
        assert!(
            actual == expected,
            "expected {expected} {label} notification(s), found {actual}\n{}",
            self.summary()
        );
        self
    }

    fn summary(self) -> String {
        let mut out = format!("notifications ({}):", self.notifications.len());
        for (index, notification) in self.notifications.iter().enumerate() {
            out.push_str(&format!("\n  [{index}] {}", describe(notification)));
        }
        out
    }
}

/// Renders one notification as a compact, family-tagged line for diagnostics.
fn describe(notification: &Notification) -> String {
    match notification {
        Notification::Llm(_) => "llm(stream event)".to_owned(),
        Notification::StepBoundary(boundary) => {
            let keys = boundary
                .metadata()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            format!("step_boundary(step={}, meta=[{keys}])", boundary.step_id())
        }
        Notification::ToolCallStarted(started) => {
            format!("tool_started(call={})", started.call_id())
        }
        Notification::ToolCallFinished(finished) => {
            format!("tool_finished(call={})", finished.call_id())
        }
        Notification::ExternalAgent(event) => {
            format!("external_agent({event:?})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::assert_notifications;
    use crate::fixtures::{
        agent_spec_with_tools, agent_state, default_machine, root_context, tool_call, weather_tool,
    };
    use crate::handlers::{ScriptedLlmHandler, ScriptedToolHandler};
    use crate::harness::DrainHarness;
    use crate::ids::SeqIds;
    use crate::scope::TestScope;
    use crate::script::{LlmStep, ToolStep};
    use serde_json::json;
    use std::sync::Arc;

    /// Drains a weather turn and returns the notifications it produced.
    async fn weather_notifications() -> Vec<agent_lib::agent::Notification> {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let spec = agent_spec_with_tools(&ids, vec![weather_tool()]);
        let machine = default_machine(&ids, agent_state(&ids, spec));

        let llm = ScriptedLlmHandler::from_steps([
            LlmStep::tool_use(vec![tool_call(
                "call-weather",
                "get_weather",
                json!({ "city": "SH" }),
            )]),
            LlmStep::text("sunny"),
        ]);
        let tool = ScriptedToolHandler::from_steps([ToolStep::ok("call-weather", "sunny")]);
        let scope = TestScope::builder()
            .llm(Arc::new(llm))
            .tool(Arc::new(tool))
            .build();

        let mut harness = DrainHarness::with_ids(machine, &scope, None, &ctx, ids);
        let observed = harness.run_user("weather?").await.expect("turn drains");
        observed.notifications().to_vec()
    }

    #[tokio::test]
    async fn happy_path_covers_tool_and_boundary_notifications() {
        let notifications = weather_notifications().await;
        let assertions = assert_notifications(&notifications);
        let started = assertions.tool_started_calls();
        assert_eq!(started.len(), 1, "one get_weather call started");

        assertions
            .tool_started_count(1)
            .tool_finished_count(1)
            .tool_started(started[0])
            .tool_finished(started[0])
            .started_then_finished(started[0]);
    }

    #[tokio::test]
    async fn missing_tool_started_failure_message_lists_stream() {
        let notifications = weather_notifications().await;
        // A call id that never appears in the stream.
        let missing: agent_lib::conversation::ToolCallId =
            "018f0d9c-7b6a-7c12-8f31-1234567890ab".parse().unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_notifications(&notifications).tool_started(missing);
        }))
        .expect_err("a missing tool-started must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("expected a tool-started notification for call"),
            "message names the expectation: {message}"
        );
        assert!(
            message.contains("tool_started("),
            "message lists the actual stream: {message}"
        );
    }

    #[test]
    fn external_agent_family_is_counted_and_accessible_in_order() {
        use agent_lib::agent::{ExternalAgentEvent, Notification};

        // A stream interleaving an external-agent event with other families: the
        // external-agent assertions must count and extract only its own family,
        // preserving stream order.
        let notifications = vec![
            Notification::ExternalAgent(ExternalAgentEvent::SessionStarted {
                session_id: Some("s-1".to_owned()),
            }),
            Notification::ExternalAgent(ExternalAgentEvent::TextDelta {
                text: "hello".to_owned(),
            }),
            Notification::ExternalAgent(ExternalAgentEvent::SessionCompleted),
        ];

        let assertions = assert_notifications(&notifications).external_agent_count(3);
        let events = assertions.external_agent_events();
        assert_eq!(events.len(), 3, "every external-agent event is extracted");
        assert!(
            matches!(
                events[0],
                ExternalAgentEvent::SessionStarted { session_id }
                    if session_id.as_deref() == Some("s-1")
            ),
            "accessor preserves stream order for the first event"
        );
        assert!(
            matches!(events[2], ExternalAgentEvent::SessionCompleted),
            "accessor preserves stream order for the last event"
        );
    }

    #[test]
    fn external_agent_count_mismatch_lists_stream() {
        use agent_lib::agent::{ExternalAgentEvent, Notification};

        let notifications = vec![Notification::ExternalAgent(
            ExternalAgentEvent::SessionCompleted,
        )];
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_notifications(&notifications).external_agent_count(2);
        }))
        .expect_err("a wrong external-agent count must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("expected 2 external agent notification(s), found 1"),
            "message names the expectation: {message}"
        );
        assert!(
            message.contains("external_agent("),
            "message lists the actual stream: {message}"
        );
    }
}
