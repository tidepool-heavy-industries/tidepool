# Wave22 inter-wave batch

Implemented the approved moderate memory batch:

1. Local actor retirement after brief kaizen, exact-source review, integration and
   focused checks. Collectors do not retire workers; a pending member blocks its
   entire group. Separate group handles retain independent cleanup boundaries.
2. External run observer: 15s cgroup/process/log metadata, 60s PSS/SwapPSS and
   passive actor roster. Output capped at20MiB; run and unrelated shared consumers
   are separated. No model calls, Haskell evaluation or process control.
3. Actor admission uses both host headroom and the minimum finite ancestor
   memory.high/max headroom, less concurrent startup reservations. It reports the
   actual limiting cgroup. No actor lifetime reservation, suspension or scheduler
   redesign is claimed.

## Review and verification

Reviewed initial and repaired admission candidates f6b1a2e/8a8bf70; integrated
3912cd586/5173ee920. Reviewed retirement 2e6220a/5248b927; integrated core256e9a140,
workspace5248b927 and pin/catalog f93b87aac. Reviewed observer ab2ca289/745018351;
integrated34a9e7285/96131bd2e. Review corrected limiting-ancestor diagnostics,
host-versus-compiler attribution, missing shared-compiler PSS and overlapping
cgroup sums. Three bounded Sol implementation agents were used; root owned final
review, source/prompt integration, lifecycle operations and launch.

- `just exomonad-build`: passed matched extractor/worker/runtime build; incremental
  Rust phase16.07s. This is build time, not notebook latency.
- `bash scripts/dev-shell.sh cargo check -p exomonad-node --tests`: passed;
  compiled affected test target, executed no tests.
- `rustfmt --edition 2021 --check` on changed Rust owners: passed. Initial bare
  rustfmt failed for missing edition; corrected command passed.
- `python3 -m py_compile exomonad/scripts/resource-observer.py`: passed.
- Agent's read-only observer `--once` on stopped wave21: matched run record,
  absent host, two old compiler processes in other_slice with about5.6GB SwapPSS.
  Live attribution and actor probe are next-run observations, not yet proven.
- Actual wave22 workspace `exomonad check`: definitions and all child-role effect
  requirements passed. No recipes or provider behavior executed by that command.
- Catalogv46 fingerprint recomputed with the production length-framed BLAKE3
  algorithm. Pin/template copied from published exact workspace5248b927.
- No new tests, synthetic benchmarks, full gate or shared compiler restart.

## Interpretation and remaining questions

The wave21 teardown reduced memory pressure without changing compiler/JIT
algorithms. Startup admission prevents a known host-versus-cgroup blind spot;
it does not prevent existing clients, JIT images or compiler state growing later.
Next-run samples should distinguish local resource release from those retained
owners before proposing another memory mechanism.

The shared-harness direction is now recorded in
plans/harness-adoption-reconciliation.md: one instance for all actors, with
independent authority/lifecycle. Savings remain a hypothesis to measure.
