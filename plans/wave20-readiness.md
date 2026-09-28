# Wave20 readiness

Updated 2026-09-28 UTC. Launch is authorized after the remaining gates.
No wave20 run has started. No recurring timer is active.

## Integrated and checked

- Git-aware source import and shared copy admission: `6360d2329`,
  `476f7228e`, `4c46712e8`. Concurrent admission and process-death lock release
  each passed a focused test.
- Retired checkout layers, recovery and last-reference release are integrated.
  The production four-child fork/retire test passed; the separate 16-manifest
  unit test is not a production storage measurement. Durable state now lives
  outside the disposable cache (`44f418d98`); legacy discovery is fenced.
- Context checkpoints, explicit release and fallible record sends are
  integrated. Six combined facade checks passed at `44f418d98` (535 skipped,
  52.088 seconds), including the hosted deferred-checkpoint and mailbox cases.
  Checkpoint sponsor accounting was subsequently repaired at `65cfd1408`;
  sponsor-budget and configured-spec checks passed, two tests total.
- Coherent run tooling and historical product-source isolation are integrated
  through `72efbbca4`; six focused source/recovery checks passed. Catalog v44
  and fingerprint were fixed at `ff5c3babe`, with its exact test passing.
- The local check wrapper preserves validated paired extractor overrides and
  starts/reuses its compiler daemon (`840d933e8`; 28 shell checks passed).
  The earlier direct-binary workaround started a cold GHC worker per request.
  The corrected path reuses worker 0: one observed cold request was 56.935s,
  followed by requests of 2.253s, 7.035s, 0.973s and 12.514s. These are individual
  compiler requests, not full recipe durations or a uniform latency promise.

## Remaining gates

1. Finish the authored WorkPlan hosted correction rehearsal. Its structural
   start, typed parallel join and terminal close have passed. Notebook fixture
   display errors and asynchronous observation ordering were repaired.
2. Repair review-discovered coordinator callback ownership: parallel verification
   cannot overwrite a single pending slot; repeated review of one developed
   candidate cannot reuse an ambiguous completion key. Check admission accounting
   before releasing checkpoint leases. Pin only the verified final candidate.
3. Integrate the shared workspace, update the scaffold pin and shipped template,
   and pin the wave20 checkout. Execute focused pin and launch-workspace checks
   using the matched local runtime and one compiler slot.
4. Launch and verify actual root admission/activity, then record all revisions,
   binary identity, run ID and log path. Do not infer launch from a tmux session.

The coordinator exposes exact review-flow handles for the original owner to
clean up. It cannot impersonate that owner. Terminal plan completion and that
explicit external cleanup step remain distinct.

## Product preparation

`/home/inanna/dev/exomonad-harness-runs/wave20`, branch `rsi/wave20`, starts from
wave19 checkpoint `d22c5a525adb104eedece0fb9fce373520c8b7ad`. Its brief and NEXT
are committed (`f7e487c`, `c553a6b`). See
[wave20 preparation](../docs/reports/wave20-preparation.md).

Sol Medium first reconciles surviving wave19 candidates and freezes the shared
contract, then coordinates three parallel Luna component trees: deterministic
asynchronous custom cells, typed job-side agent operations, and reusable tree
lifecycle. These use standalone stubs; production Exomonad integration and
credentialed inference remain outside this assignment. One bounded real
WorkPlan episode measures coordination value with ordinary orchestration as a
fallback.

Wave19 root and worker panes are dead; its compiler remains alive. The root
interview is unavailable after the crash. Dirty wave19 NEXT, trial notes,
friction notes and surviving commits are preserved. Do not restart shared
daemons, discard work, or treat this checkpoint as an accepted integrated release.

## Compiler recycling observation

The matching persistent daemon started at 2026-09-28 01:11:14 UTC with one
worker and a 7168 MiB RSS ceiling. It recycled workers at 01:13:34 (7478 MiB,
8 requests) and 01:16:54 (7376 MiB, 18 requests). Request
`d9aba353205d11ad` then took 52.760 seconds: its 139-module compilation reported
51.211 seconds wall time, including 26.675 seconds lowering, 13.426 seconds
interfaces and 5.320 seconds typechecking. Evidence is in
`~/.cache/tidepool/battery-daemon/compiler.log` and the retained test artifacts.

Daemon reuse fixes the accidental fresh-worker-per-call path; it does not
remove memory-triggered recycling. Follow up on retained compiler state and
unnecessary module loading after launch. Do not infer a uniform notebook
latency from the observed 0.3–4-second warm requests, or raise the memory limit
without considering simultaneous run/compiler memory.
