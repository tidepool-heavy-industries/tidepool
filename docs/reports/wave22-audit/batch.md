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


## Launched

Wave22 run `fc8dae5b-b02e-4884-959c-12cc060b1b8c`, tmux `wave22`, root `1@1`,
thread `01a0e8c5-f128-7160-8ead-03e20b0a7eb3`. Ready at16:08:02UTC; initial brief
accepted about16:13UTC. Root read NEXT.md and admitted four component owners
through `unfoldWork`, with retained wave21 candidates in their assignments.
Product completion remains open. The first admission cell took20.481s; this is
an observed whole-cell time, not an isolated compiler measurement.

Five WIP recovery commits now preserve the identified uncommitted changes:
actor21 `6b1c98ce00a47264d3be4f6cd913838355072f2b`, actor32
`706ebcfea2dc5a003c05ba453bb9fb41df042426`, actor3
`82dc641e3dabdb1278427fd800d9b77d17604c73`, actor12
`70fdab91fcc64414b74af8715c2dfcae9ffd6628`, actor11
`18f6ea92bfb299410945d89a09d2ccb949db6eb9`. Each has its original actor HEAD
as parent and matches its retained upper blob. These were not built or tested;
wave22 owns review and integration. Original overlays remain intact.

External observer unit `exomonad-resource-observer-wave22.service` runs in
app.slice, outside the workload. JSONL and compact summary live in the run root.
The first service launch lacked Python on systemd's PATH; relaunched with the
absolute configured interpreter. Live inspection caught native client scopes
missing from attribution; commit88f149b58 now matches the run-frozen executable
plus the exact process-supervisor subcommand, excluding the shared resource
service. Samples before16:14:50UTC misclassify the root client as other_slice;
use the corrected samples for comparisons. The live actor roster returned HTTP200.
`initial-resources.jsonl` retains the corrected first full sample. No pressure
warning at that observation; startup alone does not prove sustained capacity.
