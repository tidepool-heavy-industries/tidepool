//! A typed monotonic id issuer — the shared mechanism behind every
//! `<prefix>_<n>`-shaped id in the workspace (`tidepool-mcp`'s
//! `server_common::mint_id` and its REPL continuation-id caller,
//! `tidepool-runtime`'s `SessionEngine::next_continuation_id` and
//! `ResidentSession::next_cont_id`).
//!
//! Each of those independently paired an `AtomicU64` counter with a
//! `format!("{prefix}_{n}")` call. [`MonotonicIdIssuer`] owns both halves so
//! the scheme (start at 1, increment by one, `prefix_n` rendering) exists
//! once; callers keep their own choice of prefix and their own decision
//! about where the counter lives (bare, `Arc`-shared, …).
//!
//! Durable, cross-process-unique ids (e.g. the self-iterating harness's run
//! ids) are explicitly OUT of scope here — this is only the in-process,
//! reset-on-restart counter shape.

use std::sync::atomic::{AtomicU64, Ordering};

/// Mints ids shaped `<prefix>_<n>`, `n` starting at 1 and incrementing by one
/// per call. `Send + Sync`.
#[derive(Debug)]
pub struct MonotonicIdIssuer {
    next: AtomicU64,
    prefix: String,
}

impl MonotonicIdIssuer {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            next: AtomicU64::new(1),
            prefix: prefix.into(),
        }
    }

    /// The next id: `<prefix>_<n>`.
    pub fn next_id(&self) -> String {
        format!("{}_{}", self.prefix, self.next_raw())
    }

    /// The next raw counter value, with no prefix formatting — for a caller
    /// that mints OTHER ids off the same monotonic sequence (e.g.
    /// `ResidentSession` also derives throwaway realm ids from its
    /// continuation-id counter).
    pub fn next_raw(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_id_uses_prefix_and_increments() {
        let issuer = MonotonicIdIssuer::new("cont");
        assert_eq!(issuer.next_id(), "cont_1");
        assert_eq!(issuer.next_id(), "cont_2");
    }

    #[test]
    fn next_raw_and_next_id_share_one_sequence() {
        let issuer = MonotonicIdIssuer::new("scont");
        assert_eq!(issuer.next_raw(), 1);
        assert_eq!(issuer.next_id(), "scont_2");
        assert_eq!(issuer.next_raw(), 3);
    }
}
