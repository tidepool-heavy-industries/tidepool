//! Minimal one-shot eval probe for `scripts/bench-turn.sh`'s COLD/WARM rows.
//!
//! Runs ONE small, representative `compile_and_run_pure`-shaped eval (via
//! [`tidepool_testing::eval_harness::EvalHarness`], the established
//! production-path test wrapper — no new compile/run plumbing here) and
//! prints its wall clock to stdout as a `key=value` line. COLD vs WARM is
//! controlled entirely from OUTSIDE this process: the caller sets
//! `TIDEPOOL_COMPILE_CACHE_DIR` to a fresh directory (cold) or an already-warm
//! one (warm) before invoking this binary — `compile_haskell`'s on-disk memo
//! is content-addressed, so a second PROCESS against the same cache dir still
//! hits it.
//!
//! With `TIDEPOOL_TIMING=1` set, a cache-miss run's `tidepool-extract` spawn
//! writes `tidepool-timing phase=<name> ms=<n>` lines to its stderr, which
//! `compile_haskell` forwards verbatim to this process's OWN stderr — the
//! caller greps those out directly (see `timing::TIMING_PREFIX`); a
//! cache-hit (WARM) run spawns no extract process, so no phase lines appear,
//! which is the expected signal that nothing was recompiled.
//!
//! Run directly: `cargo run --release --example bench_oneshot -p tidepool-runtime`
//! (needs `TIDEPOOL_EXTRACT` + a with-packages GHC on `PATH`).

use std::time::Instant;

use tidepool_testing::eval_harness::{require_extract, EvalHarness};

fn main() {
    require_extract();

    // Small but non-trivial: a list build + fold, cheap enough that GHC
    // compile time (not JIT run time) dominates the wall clock — which is
    // what a one-shot eval's cost profile actually looks like.
    let source = "module BenchOneshot where\nresult :: Int\nresult = sum [1 .. (5000 :: Int)]\n";

    let start = Instant::now();
    let outcome = EvalHarness::new().with_stdlib().run_pure(source, "result");
    let wall_ms = start.elapsed().as_millis();

    let result = outcome.expect("bench_oneshot eval must succeed");
    println!("wall_ms={wall_ms}");
    println!("value={}", result.to_json());
}
