//! Read-only assertions over a scripted handler's [`CallLog`].
//!
//! [`assert_calls`] wraps the `&CallLog` a scripted handler exposes through its
//! `log()` accessor and lets a test assert on how many calls began, how many
//! completed, the completion order (dispatch index -> completion index), and the
//! peak concurrency the log observed.

use crate::script::CallLog;

/// Starts a fluent, read-only assertion over a handler call log.
#[must_use]
pub fn assert_calls<Req, Res>(log: &CallLog<Req, Res>) -> CallAssertions<'_, Req, Res> {
    CallAssertions { log }
}

/// A fluent, read-only assertion builder over a [`CallLog`].
pub struct CallAssertions<'a, Req, Res> {
    log: &'a CallLog<Req, Res>,
}

impl<Req, Res> Clone for CallAssertions<'_, Req, Res> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Req, Res> Copy for CallAssertions<'_, Req, Res> {}

impl<'a, Req, Res> CallAssertions<'a, Req, Res> {
    /// Asserts the number of calls that began (dispatch count).
    pub fn count(self, expected: usize) -> Self {
        let actual = self.log.len();
        assert!(
            actual == expected,
            "expected {expected} call(s) to have begun, found {actual}{}",
            self.suffix()
        );
        self
    }

    /// Asserts the number of requests captured, which equals the number of
    /// calls that began (each begin records one request).
    pub fn request_count(self, expected: usize) -> Self {
        let actual = self.log.len();
        assert!(
            actual == expected,
            "expected {expected} request(s) captured, found {actual}{}",
            self.suffix()
        );
        self
    }

    /// Asserts the number of calls that completed.
    pub fn completed(self, expected: usize) -> Self {
        let actual = self.log.completed_len();
        assert!(
            actual == expected,
            "expected {expected} completed call(s), found {actual}{}",
            self.suffix()
        );
        self
    }

    /// Asserts every begun call has completed.
    pub fn all_completed(self) -> Self {
        let begun = self.log.len();
        let completed = self.log.completed_len();
        assert!(
            begun == completed,
            "expected all {begun} call(s) to have completed, but only {completed} did{}",
            self.suffix()
        );
        self
    }

    /// Asserts the completion order: for each dispatch index `i`, `expected[i]`
    /// is the zero-based order in which that call completed. Requires every call
    /// to have completed.
    pub fn completion_order(self, expected: &[usize]) -> Self {
        let actual = self.log.with_records(|records| {
            records
                .iter()
                .map(|r| r.completion_index)
                .collect::<Vec<_>>()
        });
        if actual.len() != expected.len() {
            panic!(
                "expected completion order over {} call(s), but {} call(s) were dispatched (actual completion indices: {actual:?})",
                expected.len(),
                actual.len(),
            );
        }
        for (dispatch_index, slot) in actual.iter().enumerate() {
            match slot {
                Some(completion_index) => assert!(
                    *completion_index == expected[dispatch_index],
                    "call dispatched at {dispatch_index} completed at {completion_index}, expected {}\n  full completion order (dispatch -> completion): {actual:?}",
                    expected[dispatch_index]
                ),
                None => panic!(
                    "call dispatched at {dispatch_index} has not completed yet\n  full completion order (dispatch -> completion): {actual:?}"
                ),
            }
        }
        self
    }

    /// Asserts the peak number of calls in flight at once.
    pub fn peak_concurrency(self, expected: usize) -> Self {
        let actual = self.log.peak_concurrency();
        assert!(
            actual == expected,
            "expected peak concurrency {expected}, found {actual}{}",
            self.suffix()
        );
        self
    }

    fn suffix(self) -> String {
        format!(
            " (begun={}, completed={}, peak={})",
            self.log.len(),
            self.log.completed_len(),
            self.log.peak_concurrency(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::assert_calls;
    use crate::script::CallLog;

    #[test]
    fn happy_path_covers_counts_order_and_peak() {
        let log: CallLog<u8, u8> = CallLog::new();
        // Two overlapping calls that complete out of dispatch order.
        let a = log.begin(1);
        let b = log.begin(2);
        log.complete(b, 20);
        log.complete(a, 10);

        assert_calls(&log)
            .count(2)
            .request_count(2)
            .completed(2)
            .all_completed()
            .peak_concurrency(2)
            // dispatch 0 (a) completed second (index 1); dispatch 1 (b) first (0).
            .completion_order(&[1, 0]);
    }

    #[test]
    fn peak_concurrency_failure_message_reports_actual() {
        let log: CallLog<u8, u8> = CallLog::new();
        log.record(1, 10);

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_calls(&log).peak_concurrency(3);
        }))
        .expect_err("a wrong peak must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("expected peak concurrency 3, found 1"),
            "message names expected and actual: {message}"
        );
        assert!(
            message.contains("begun=1, completed=1, peak=1"),
            "message includes a log summary: {message}"
        );
    }

    #[test]
    fn completion_order_incomplete_call_panics_with_indices() {
        let log: CallLog<u8, u8> = CallLog::new();
        log.begin(1); // dispatched but never completed
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_calls(&log).completion_order(&[0]);
        }))
        .expect_err("an incomplete call must panic");
        let message = panic
            .downcast_ref::<String>()
            .expect("panic payload is a String");
        assert!(
            message.contains("has not completed yet"),
            "message flags the incomplete call: {message}"
        );
    }
}
