//! Read-only assertions over terminal budget accounting.
//!
//! [`assert_budget`] snapshots [`RunContext::budget().snapshot()`] and lets a
//! test assert on consumed steps, tokens, and cost. [`assert_budget_snapshot`]
//! works from a previously captured [`BudgetSnapshot`].

use agent_lib::agent::{BudgetSnapshot, RunContext};

/// Starts a fluent, read-only assertion over `ctx`'s consumed budget.
#[must_use]
pub fn assert_budget(ctx: &RunContext) -> BudgetAssertions {
    assert_budget_snapshot(&ctx.budget().snapshot())
}

/// Starts a fluent, read-only assertion over a captured budget snapshot.
#[must_use]
pub fn assert_budget_snapshot(snapshot: &BudgetSnapshot) -> BudgetAssertions {
    BudgetAssertions {
        snapshot: *snapshot,
    }
}

/// A fluent, read-only assertion builder over a [`BudgetSnapshot`].
#[derive(Clone, Copy)]
pub struct BudgetAssertions {
    snapshot: BudgetSnapshot,
}

impl BudgetAssertions {
    /// Returns the underlying snapshot for escape-hatch inspection.
    pub const fn snapshot(self) -> BudgetSnapshot {
        self.snapshot
    }

    /// Asserts the number of consumed logical agent steps.
    pub fn steps(self, expected: u64) -> Self {
        let actual = self.snapshot.used().steps();
        assert!(
            actual == expected,
            "expected {expected} consumed step(s), found {actual} ({})",
            self.summary()
        );
        self
    }

    /// Asserts the number of consumed model tokens.
    pub fn tokens(self, expected: u64) -> Self {
        let actual = self.snapshot.used().tokens();
        assert!(
            actual == expected,
            "expected {expected} consumed token(s), found {actual} ({})",
            self.summary()
        );
        self
    }

    /// Asserts the consumed cost in micro-units.
    pub fn cost_micros(self, expected: u64) -> Self {
        let actual = self.snapshot.used().cost_micros();
        assert!(
            actual == expected,
            "expected {expected} consumed cost micro-unit(s), found {actual} ({})",
            self.summary()
        );
        self
    }

    fn summary(self) -> String {
        let used = self.snapshot.used();
        format!(
            "used steps={}, tokens={}, cost_micros={}",
            used.steps(),
            used.tokens(),
            used.cost_micros()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::assert_budget;
    use crate::fixtures::root_context;
    use crate::ids::SeqIds;

    #[test]
    fn happy_path_reports_charged_dimensions() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        ctx.charge_step()
            .expect("step charge fits the unbounded budget");
        ctx.charge_step()
            .expect("step charge fits the unbounded budget");
        ctx.charge_tokens(64).expect("token charge fits");
        ctx.charge_cost_micros(1_000).expect("cost charge fits");

        assert_budget(&ctx).steps(2).tokens(64).cost_micros(1_000);
    }

    #[test]
    fn tokens_failure_message_reports_actual() {
        let ids = SeqIds::new();
        let ctx = root_context(&ids);
        ctx.charge_tokens(10).expect("token charge fits");

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_budget(&ctx).tokens(99);
        }))
        .expect_err("a wrong token count must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("expected 99 consumed token(s), found 10"),
            "message names expected and actual: {message}"
        );
    }
}
