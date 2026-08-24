# tidepool-testing — property-based generators + integration-test harness

**Charter.** Belongs: proptest generators for well-typed `CoreExpr` values,
and `EvalHarness` — the shared setup (include path, eval thread, effect
preamble, mock handler stack) `tidepool-runtime`/`tidepool-repl` integration
tests build on top of, driving the real `compile_and_run`/`compile_haskell`
path. Does NOT belong: production effect handlers (`tidepool-handlers`),
bridged wire-record types (`tidepool-bridge-effects`, imported here, not
defined here).
