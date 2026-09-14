# tidepool-testing — property-based generators + integration-test harness

**Charter.** Belongs: proptest generators for well-typed `CoreExpr` values,
and `EvalHarness` — the shared setup (include path, eval thread, effect
preamble, mock handler stack) `tidepool-runtime` integration tests build on
top of, driving the real `compile_and_run`/`compile_haskell` path (also used
by `tidepool-repl` integration tests when that crate is a workspace member —
currently it is not; its `Cargo.toml` was removed under the in-progress STG
cutover). Does NOT belong: production effect handlers (`tidepool-handlers`),
bridged wire-record types (`tidepool-bridge-effects`, imported here, not
defined here).
