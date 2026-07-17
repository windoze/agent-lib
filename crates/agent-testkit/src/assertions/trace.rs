//! Read-only assertions over the [`RunContext`] trace tree.
//!
//! [`assert_trace`] snapshots [`RunContext::trace().records()`] and lets a test
//! assert on the settled-requirement nodes (family, `resolved_at_scope`,
//! disposition) and on the parent chain of any node (for example a subagent's
//! parentage). The current driver records a [`TraceNodeKind::Requirement`] node
//! per settled requirement, keyed by the requirement's own id, plus the run
//! root; these assertions start from that and grow with trace granularity.

use agent_lib::agent::{
    RequirementDisposition, RequirementId, RequirementKindTag, RunContext, TraceNodeId,
    TraceNodeKind, TraceRecord,
};

/// Starts a fluent, read-only assertion over `ctx`'s trace tree.
#[must_use]
pub fn assert_trace(ctx: &RunContext) -> TraceAssertions {
    assert_trace_records(ctx.trace().records())
}

/// Starts a fluent, read-only assertion over an owned set of trace records.
#[must_use]
pub fn assert_trace_records(records: Vec<TraceRecord>) -> TraceAssertions {
    TraceAssertions { records }
}

/// A fluent, read-only assertion builder over a snapshot of trace records.
///
/// It owns the snapshot, so it does not borrow the [`RunContext`] and cannot
/// perturb ongoing recording. Count assertions return `&Self` for chaining;
/// `requirement`/`node` return an owned drill-down view.
pub struct TraceAssertions {
    records: Vec<TraceRecord>,
}

impl TraceAssertions {
    /// Returns the snapshot of trace records.
    pub fn records(&self) -> &[TraceRecord] {
        &self.records
    }

    /// Asserts the total number of recorded trace nodes (including the root).
    pub fn node_count(&self, expected: usize) -> &Self {
        let actual = self.records.len();
        assert!(
            actual == expected,
            "expected {expected} trace node(s), found {actual}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts the number of settled-requirement trace nodes.
    pub fn requirement_count(&self, expected: usize) -> &Self {
        let actual = self
            .records
            .iter()
            .filter(|record| matches!(record.kind(), TraceNodeKind::Requirement { .. }))
            .count();
        assert!(
            actual == expected,
            "expected {expected} requirement trace node(s), found {actual}\n{}",
            self.summary()
        );
        self
    }

    /// Asserts the number of subagent trace nodes.
    pub fn subagent_count(&self, expected: usize) -> &Self {
        let actual = self
            .records
            .iter()
            .filter(|record| matches!(record.kind(), TraceNodeKind::SubAgent))
            .count();
        assert!(
            actual == expected,
            "expected {expected} subagent trace node(s), found {actual}\n{}",
            self.summary()
        );
        self
    }

    /// Finds the settled-requirement node keyed by `id` and returns a view.
    ///
    /// The driver keys a requirement's trace node by the requirement's own id,
    /// so this matches the node whose id string equals `id`.
    pub fn requirement(&self, id: RequirementId) -> RequirementTraceView {
        let node_id = id.to_string();
        let record = self
            .records
            .iter()
            .find(|record| {
                record.id().as_str() == node_id
                    && matches!(record.kind(), TraceNodeKind::Requirement { .. })
            })
            .unwrap_or_else(|| {
                panic!(
                    "expected a settled-requirement trace node for id {id}\n{}",
                    self.summary()
                )
            });
        RequirementTraceView {
            record: record.clone(),
        }
    }

    /// Finds any node by its [`TraceNodeId`] and returns a view over it.
    pub fn node(&self, id: &TraceNodeId) -> TraceNodeView {
        let record = self
            .records
            .iter()
            .find(|record| record.id() == id)
            .unwrap_or_else(|| {
                panic!(
                    "expected a trace node with id {}\n{}",
                    id.as_str(),
                    self.summary()
                )
            });
        TraceNodeView {
            record: record.clone(),
        }
    }

    fn summary(&self) -> String {
        let mut out = format!("trace ({} node(s)):", self.records.len());
        for record in &self.records {
            out.push_str(&format!(
                "\n  {} <- {} : {}",
                record.id().as_str(),
                record
                    .parent()
                    .map_or("<root>", agent_lib::agent::TraceNodeId::as_str),
                describe_kind(record.kind()),
            ));
        }
        out
    }
}

/// A read-only view over one settled-requirement trace node.
#[derive(Clone)]
pub struct RequirementTraceView {
    record: TraceRecord,
}

impl RequirementTraceView {
    /// Returns the underlying trace record.
    pub const fn record(&self) -> &TraceRecord {
        &self.record
    }

    /// Asserts the requirement family recorded on this node.
    pub fn tag(self, expected: RequirementKindTag) -> Self {
        let actual = self.kind_tag();
        assert!(
            actual == expected,
            "expected requirement trace family `{expected}`, found `{actual}` on node {}",
            self.record.id().as_str()
        );
        self
    }

    /// Asserts the pop distance from the performing layer to the resolving scope.
    pub fn resolved_at_scope(self, expected: u32) -> Self {
        let actual = self.scope();
        assert!(
            actual == expected,
            "expected resolved_at_scope {expected}, found {actual} on node {}",
            self.record.id().as_str()
        );
        self
    }

    /// Asserts the recorded disposition.
    pub fn disposition(self, expected: RequirementDisposition) -> Self {
        let actual = self.disposition_value();
        assert!(
            actual == expected,
            "expected disposition {expected:?}, found {actual:?} on node {}",
            self.record.id().as_str()
        );
        self
    }

    /// Asserts the requirement was resumed by a handler.
    pub fn resumed(self) -> Self {
        self.disposition(RequirementDisposition::Resumed)
    }

    /// Asserts the requirement's continuation was abandoned (never resumed).
    pub fn never_resumed(self) -> Self {
        self.disposition(RequirementDisposition::NeverResumed)
    }

    fn requirement_kind(&self) -> (RequirementKindTag, u32, RequirementDisposition) {
        match self.record.kind() {
            TraceNodeKind::Requirement {
                kind_tag,
                resolved_at_scope,
                disposition,
            } => (kind_tag, resolved_at_scope, disposition),
            other => panic!(
                "trace node {} is not a requirement node: {}",
                self.record.id().as_str(),
                describe_kind(other)
            ),
        }
    }

    fn kind_tag(&self) -> RequirementKindTag {
        self.requirement_kind().0
    }

    fn scope(&self) -> u32 {
        self.requirement_kind().1
    }

    fn disposition_value(&self) -> RequirementDisposition {
        self.requirement_kind().2
    }
}

/// A read-only view over any single trace node, used for parent-chain checks.
#[derive(Clone)]
pub struct TraceNodeView {
    record: TraceRecord,
}

impl TraceNodeView {
    /// Returns the underlying trace record.
    pub const fn record(&self) -> &TraceRecord {
        &self.record
    }

    /// Asserts this node's parent node id (or that it is the root when `None`).
    pub fn parent_is(self, expected: Option<&TraceNodeId>) -> Self {
        let actual = self.record.parent();
        assert!(
            actual == expected,
            "expected parent {:?}, found {:?} for node {}",
            expected.map(TraceNodeId::as_str),
            actual.map(TraceNodeId::as_str),
            self.record.id().as_str()
        );
        self
    }

    /// Asserts this node's kind.
    pub fn kind_is(self, expected: TraceNodeKind) -> Self {
        let actual = self.record.kind();
        assert!(
            actual == expected,
            "expected kind {}, found {} for node {}",
            describe_kind(expected),
            describe_kind(actual),
            self.record.id().as_str()
        );
        self
    }
}

/// Renders a trace node kind compactly for diagnostics.
fn describe_kind(kind: TraceNodeKind) -> String {
    match kind {
        TraceNodeKind::Run => "run".to_owned(),
        TraceNodeKind::Step => "step".to_owned(),
        TraceNodeKind::Llm => "llm".to_owned(),
        TraceNodeKind::Tool => "tool".to_owned(),
        TraceNodeKind::SubAgent => "subagent".to_owned(),
        TraceNodeKind::Requirement {
            kind_tag,
            resolved_at_scope,
            disposition,
        } => format!("requirement({kind_tag}, scope={resolved_at_scope}, {disposition:?})"),
        TraceNodeKind::ExternalShutdown { disposition } => {
            format!("external_shutdown({})", disposition.label())
        }
        TraceNodeKind::ExternalUsage {
            tokens_charged,
            cost_micros_charged,
        } => format!(
            "external_usage(tokens={tokens_charged:?}, cost_micros={cost_micros_charged:?})"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{assert_trace, assert_trace_records};
    use crate::fixtures::{
        agent_spec_with_tools, agent_state, assistant_text, default_machine, root_context, usage,
        user_input,
    };
    use crate::handlers::ScriptedLlmHandler;
    use crate::ids::SeqIds;
    use crate::scope::TestScope;
    use crate::script::LlmStep;
    use agent_lib::agent::{
        RequirementDisposition, RequirementId, RequirementKindTag, TraceNodeKind, drain,
    };
    use std::sync::Arc;

    /// Drives a text-only turn and returns the run context so its trace can be
    /// asserted on. The turn settles exactly one `NeedLlm` requirement.
    async fn text_turn_ctx() -> agent_lib::agent::RunContext {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        let spec = agent_spec_with_tools(&ids, vec![]);
        let mut machine = default_machine(&ids, agent_state(&ids, spec));

        let llm =
            ScriptedLlmHandler::from_steps([LlmStep::response(assistant_text("hi", usage(3, 2)))]);
        let scope = TestScope::builder().llm(Arc::new(llm)).build();

        drain(&mut machine, user_input(&ids, "hello"), &scope, None, &ctx)
            .await
            .expect("text turn drains");
        ctx
    }

    /// Extracts the single settled-requirement node's id from a trace snapshot.
    fn only_requirement_id(records: &[agent_lib::agent::TraceRecord]) -> RequirementId {
        let node = records
            .iter()
            .find(|record| matches!(record.kind(), TraceNodeKind::Requirement { .. }))
            .expect("a settled-requirement node exists");
        node.id()
            .as_str()
            .parse()
            .expect("node id is a requirement id")
    }

    #[tokio::test]
    async fn happy_path_covers_requirement_disposition_and_scope() {
        let ctx = text_turn_ctx().await;
        let records = ctx.trace().records();
        let llm_id = only_requirement_id(&records);

        assert_trace(&ctx).requirement_count(1).subagent_count(0);
        assert_trace(&ctx)
            .requirement(llm_id)
            .tag(RequirementKindTag::Llm)
            .resolved_at_scope(0)
            .disposition(RequirementDisposition::Resumed)
            .resumed();
    }

    #[tokio::test]
    async fn missing_requirement_failure_message_lists_nodes() {
        let ctx = text_turn_ctx().await;
        let stray: RequirementId = "018f0d9c-7b6a-7c12-8f31-1234567890ab".parse().unwrap();
        let records = ctx.trace().records();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_trace_records(records).requirement(stray);
        }))
        .expect_err("a missing requirement node must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("expected a settled-requirement trace node for id"),
            "message names the expectation: {message}"
        );
        assert!(
            message.contains("requirement(llm"),
            "message lists the actual trace nodes: {message}"
        );
    }
}
