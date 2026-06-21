//! Env-gated emit-path coverage counters.
//!
//! Records which compile-time decision points the JIT emitter exercises —
//! `CoreFrame`/`EmitFrame` variant, case-dispatch shape, constructor-repr branch —
//! so the real-Core corpus runner can report a hit/unhit emit-path coverage
//! number ("are we done / where would a generator pay off").
//!
//! Zero-cost when `TIDEPOOL_EMIT_COVERAGE` is unset: every [`hit`] is one relaxed
//! atomic load + early return, and nothing is recorded. No behavior change either
//! way — purely observational.
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU8, Ordering};

static ENABLED: AtomicU8 = AtomicU8::new(0); // 0 = unknown, 1 = on, 2 = off

/// Whether coverage recording is active (read once from the environment, cached).
pub fn is_enabled() -> bool {
    match ENABLED.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("TIDEPOOL_EMIT_COVERAGE").is_some();
            ENABLED.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

thread_local! {
    static HITS: RefCell<BTreeSet<&'static str>> = const { RefCell::new(BTreeSet::new()) };
}

/// Record that emit decision-point `key` fired. No-op unless enabled.
#[inline]
pub fn hit(key: &'static str) {
    if is_enabled() {
        HITS.with(|h| {
            h.borrow_mut().insert(key);
        });
    }
}

/// Snapshot the CURRENT THREAD's hit set. The corpus runner compiles each fixture
/// on its own worker thread, so it calls this inside that thread and unions across.
pub fn snapshot() -> BTreeSet<&'static str> {
    HITS.with(|h| h.borrow().clone())
}

/// Clear the current thread's hit set (call before a fresh compile).
pub fn reset() {
    HITS.with(|h| h.borrow_mut().clear());
}

/// The full enumerable target set — every key [`hit`] can record. Kept in sync
/// with the instrumentation by hand; drives the coverage %/unhit report.
pub const TARGETS: &[&str] = &[
    // EmitFrame variants (collapse_frame) — the CoreFrame shapes the JIT lowers.
    "frame:Var",
    "frame:Lit",
    "frame:LitString",
    "frame:LitByteArray",
    "frame:Con",
    "frame:ThunkCon",
    "frame:App",
    "frame:PrimOp",
    "frame:Jump",
    "frame:Case",
    "frame:Lam",
    "frame:Join",
    "frame:LetBoundary",
    "frame:LetNonRec",
    "frame:LetRec",
    "frame:Raise",
    // case-dispatch shape
    "case:1alt",
    "case:2alt",
    "case:3+alt",
    "case:default",
    "case:dataalt",
    "case:litalt",
    // constructor-repr
    "con:nullary",
    "con:nonnullary",
];
