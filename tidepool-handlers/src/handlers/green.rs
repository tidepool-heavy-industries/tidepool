//! Green effect handler: green-thread identity and cancellation (PRD 20,
//! S1-L4 — `plans/self-iterating-harness/20-s1l4-green-threads.md`).
//!
//! A green thread IS an `M a` value — its residual computation — driven one
//! effect at a time by an ordinary Haskell round-robin (`Tidepool.Async`).
//! Two things a Haskell value cannot hold live here instead:
//!
//! - **identity**, so a thread can be named in a trace, and
//! - **cancellation status**, because `cancel a` must be observable to a
//!   later `wait a` held by someone else.
//!
//! That is the whole handler: a counter and two sets. There is no scheduler
//! here, no thread table of continuations, and no wake queue — scheduling is
//! Haskell's, at effect boundaries, over the one driver loop.

use std::collections::HashSet;

use tidepool_effect::dispatch::EffectContext;
use tidepool_effect::error::EffectError;
use tidepool_mcp::CapturedOutput;

// GreenReq + DescribeEffect + EffectHandler dispatch are generated from the
// single-source definition; only the handler struct and the per-verb method
// bodies below are hand-written.
tidepool_mcp::green_effect_def!(crate::effect_glue::effect_rust_projection);

/// Green-thread identity + cancellation status for one run.
///
/// Ids are monotonic from 1 for the lifetime of this handler instance (0 is
/// never minted, so a zero id in a trace is always a bug rather than thread
/// one). Both status sets are grow-only: a thread is a value, so nothing ever
/// un-cancels or un-settles.
#[derive(Debug, Default, Clone)]
pub struct GreenHandler {
    next_id: i64,
    cancelled: HashSet<i64>,
    settled: HashSet<i64>,
}

impl GreenHandler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every id minted so far, for a caller that wants to render the run's
    /// threads. Ascending.
    pub fn thread_ids(&self) -> Vec<i64> {
        (1..=self.next_id).collect()
    }

    /// Has `id` been cancelled? (Test/observability read; the authored path
    /// goes through the effect.)
    pub fn is_cancelled(&self, id: i64) -> bool {
        self.cancelled.contains(&id)
    }

    /// Has `id` settled — i.e. did some scheduling point drive it to a value?
    pub fn is_settled(&self, id: i64) -> bool {
        self.settled.contains(&id)
    }

    fn green_new(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<tidepool_effect::Response, EffectError> {
        self.next_id += 1;
        cx.respond(self.next_id)
    }

    /// Idempotent by construction (a set insert). Cancelling an unknown or
    /// already-settled id is deliberately NOT an error: `cancel` on a
    /// terminal handle is a no-op, exactly as `Control.Concurrent.Async`
    /// specifies.
    fn green_cancel(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        thread_id: i64,
    ) -> Result<tidepool_effect::Response, EffectError> {
        self.cancelled.insert(thread_id);
        cx.respond(())
    }

    fn green_cancelled(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        thread_id: i64,
    ) -> Result<tidepool_effect::Response, EffectError> {
        cx.respond(self.cancelled.contains(&thread_id))
    }

    fn green_settle(
        &mut self,
        cx: &EffectContext<'_, CapturedOutput>,
        thread_id: i64,
    ) -> Result<tidepool_effect::Response, EffectError> {
        self.settled.insert(thread_id);
        cx.respond(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_monotonic_from_one() {
        let mut h = GreenHandler::new();
        h.next_id += 1;
        assert_eq!(h.next_id, 1);
        h.next_id += 1;
        assert_eq!(h.next_id, 2);
        assert_eq!(h.thread_ids(), vec![1, 2]);
    }

    #[test]
    fn cancel_is_idempotent_and_readable() {
        let mut h = GreenHandler::new();
        assert!(!h.is_cancelled(7));
        h.cancelled.insert(7);
        h.cancelled.insert(7);
        assert!(h.is_cancelled(7));
        assert!(!h.is_cancelled(8));
    }

    #[test]
    fn settle_is_independent_of_cancel() {
        let mut h = GreenHandler::new();
        h.settled.insert(3);
        assert!(h.is_settled(3));
        assert!(!h.is_cancelled(3));
    }
}
