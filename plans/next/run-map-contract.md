# Run-map local boundary

`tidepool::run_map` owns bounded read-only artifact derivation and typed
Observed/Inferred/Unknown evidence. Reader implementer owns this module and
private fixtures beneath src/run_map; historical reconciler owns a separate
plans/next/run-map-reconciliation.md and any tests it adds under its own path.
No launch-owner edits. CLI wiring will be integrated through root/service owner
into existing Shoal CLI, rather than inventing a parallel launcher. Reader should
provide explicit run-directory and optional UTC window inputs plus serialized
and concise textual outputs. Malformed/missing records produce partial reports
and diagnostics. No inferred acceptance, cumulative usage summation or raw prompt
capture. Source scaffold intentionally has no successful reader stub.
