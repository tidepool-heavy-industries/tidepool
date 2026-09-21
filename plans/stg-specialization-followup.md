# STG specialization: measured follow-up

This document records measurements and the next design decisions after the
bounded specialization work. It does not authorize implementing a support
package, a general home-module cache, dispatcher redesign, region index, or
literal reclamation in this implementation batch.

## Compiler work

The initial instrumented per-unit run used the retained compiler daemon and
passed one test, with four skipped. Its body took 21.85 s. The command was:

```sh
TIDEPOOL_KEEP_TEST_LOGS=1 TIDEPOOL_TIMING=1 NEXTEST_SUCCESS_OUTPUT=immediate \
  just test-target tidepool-runtime session \
  'test(=prepared_unit_codegen_cost::a_later_unit_reuses_an_earlier_unit_s_generated_code)'
```

Source revision: `99e25f4ce`, with the corpus-script work present but unused by
this test. Evidence is the successful run directory
`target/tidepool-test-runs/20260921T193509Z-3012845-battery`. Successful logs are
bounded, so the useful measurements are recorded here. Other implementation
builds were running; these are phase observations, not controlled speedups.

| Warm request | Total | Compile summary | Recovery | Projection | Encoding | Sidecars |
|---|---:|---:|---:|---:|---:|---:|
| text | 232 ms | 59 ms | 47 ms | 101 ms | 17 ms | 5 ms |
| references | 202 ms | 44 ms | 43 ms | 98 ms | 11 ms | 5 ms |
| list | 223 ms | 44 ms | 59 ms | 101 ms | 13 ms | 5 ms |
| final expression | 245 ms | 44 ms | 60 ms | 103 ms | 12 ms | 5 ms |

Interface construction is inside the compile summary: 22–24 ms in these warm
requests. Target typechecking was 6 ms and lowering 2–15 ms. The summary and
its nested phases must not be summed together. Projection is forced only as
far as its result; encoding may force remaining work, so compare their sum.
The first two requests populated different compiler state and took 10.96 s
and 6.84 s; they are not the same warm request shape.

A later whole-cell measurement separated the two previously unexplained
compiler shapes. In recurring warm requests, interface construction was
occasionally 690–939 ms while target typechecking remained 14–17 ms. The
measured cost is specifically GHC's `mkIfaceTc`, which constructs and
fingerprints the in-memory `ModIface` needed for later generated modules to
import the declaration; it is not native code generation. Ordinary warm
requests instead spent 34–45 ms in interface construction. The next run must
attribute this phase to the owning module and distinguish CPU, allocation and
GC time before changing interface policy.

Prepared target selection was the other compiler-side concentration. Warm
requests spent 116–170 ms selecting the reachable graph and 9–12 ms lowering
it. Recovery spent 63–95 ms, mostly reachability and reference traversal.
Projection remains lazy enough that its phase and encoding must be interpreted
together in this recorded run. The integrated instrumentation now forces the
complete identity inventory in `prepared_project` and assigns lazy lowering
plus serialization together to `prepared_encode`, so future runs have an
unambiguous boundary.

The same whole-cell trace reconciled end-to-end wall time. Two bootstrap
requests preceded the measured cells and are excluded. Queue admission and
machine checkout were both 0 ms throughout. “Rust/orchestration” is the
remaining wall time after compiler-worker requests and prepared native compile
and install; it includes request serialization, artifact reads and the small
amount of turn orchestration not separately timed.

| Cell shape | Wall | Compiler worker | Attributed inside worker | Worker gap | Native compile/install | Rust/orchestration |
|---|---:|---:|---:|---:|---:|---:|
| cold first cell, 5 requests | 36,014 ms | 34,775 ms | 34,719 ms | 56 ms | 1,058 ms | 181 ms |
| warm one statement, round 0 | 2,307 ms | 1,246 ms | 1,200 ms | 46 ms | 906 ms | 155 ms |
| warm six statements, round 0 | 9,584 ms | 5,613 ms | 5,489 ms | 124 ms | 3,532 ms | 439 ms |
| warm one statement, round 1 | 2,675 ms | 1,535 ms | 1,466 ms | 69 ms | 960 ms | 180 ms |
| warm six statements, round 1 | 8,095 ms | 4,862 ms | 4,748 ms | 114 ms | 2,796 ms | 437 ms |
| unchanged lookup | 42 ms | 36 ms | 17 ms | 19 ms | 0 ms | 6 ms |

Thus warm authored cells spend roughly 54–58% in the compiler worker, 35–39%
in prepared native compilation/installation, and 5–7% in the remaining Rust
path. Within executable compiler requests, named phases leave about 6–15 ms
unclassified each; check-only requests additionally expose their GHC setup and
load time. The earlier 166 ms and 599 ms unknown buckets are resolved: ordinary
request time is principally selection/recovery, and the recurring slow shape
is `mkIfaceTc`.

Next decision: subdivide projection only where these measurements justify it,
and locate the separately observed slow whole-cell request shape. Compare
fixed support-package compilation with general home-unit reuse after measuring
which defining modules account for emitted bytes. A support package offers a
narrower lifetime contract; general home-unit reuse also has to preserve
entry-relative IDs, caller site identity, generation imports, and ownership.

## Native dispatch

Earlier observations put dispatcher emission at 22–33% of later-unit native
compilation. Measure actual demand expansion, compatible offers, functions,
blocks, and bytes before choosing pruning, shared dispatch, or a different
lookup structure. Record both total category costs and defining-module costs;
a count of tops alone does not explain generated bytes.

The detailed later-unit run expanded 15 direct demands to 27 owners and 72
closed demands, producing 72 dispatchers, 4,417 offers, 165,301 dispatcher
bytes and 609,845 native bytes overall. Other request shapes produced
695,988–918,119 native bytes. `Tidepool.Aeson.Value` alone accounted for
228,489 native bytes in the measured final unit, while the retained literal
pool was only 3,577 bytes. Category and per-definition metrics overlap by
design and must retain an explicit scope field so aggregators do not sum them.

## Memory and disk

Measure static-region probes per lookup and region counts before adding an
index. An index must preserve exact object-start and tag validation during
install rollback and retirement. Measure permanent literal bytes and entries
separately from live programs before choosing reclamation: embedded raw
addresses and cross-program deduplication constrain ownership.

Successful corpus scratch is regenerable; keep a bounded summary and remove
it. Keep full failure evidence, including frozen executables and provenance.
Distinguish apparent reflink size from physical disk consumption.

## Build and test work

Track selected Cargo targets and dependency fanout, then execution counts and
compiler requests. Do not infer a clean-build speedup from incremental timing.
Separate lightweight fixture data from heavy evaluation and corpus support.
Group fixed-fixture scenarios while preserving named assertions and fresh
machines where mutable state matters. Keep process isolation for crash,
signal, and process-global tests. Daily selections remain narrow; interface
changes compile downstream consumers, and integration runs the broad gate.

## Next Astra design wave

The next performance wave is an Astra planning pass. It will use the complete
phase evidence above rather than beginning implementation during this batch.
Its confirmed scope includes:

- replace the Haskell JSON parser and generated `__decodeValue` path with a
  typed Rust-backed JSON intrinsic, following the chrono intrinsic's exact
  source, type and constructor authority checks;
- explain `mkIfaceTc`'s variable interface cost with per-module attribution
  and GHC runtime statistics before choosing reuse or a narrower interface;
- remove duplicate selection/reachability work where the typed recovery and
  projection graphs prove they are the same decision;
- use emitted demand, offer, native-time and byte counts to choose dispatcher
  pruning or sharing; and
- evaluate static-region indexing and literal reclamation only against
  request-scoped and lifetime-scoped memory measurements.

## Candidate follow-up wave: resident Haskell compute

Audit these only after the structural compiler/dispatch/JSON wave has matched
measurements. A candidate needs a production consumer and workload evidence;
source size or a handwritten parser alone is not a reason to add an intrinsic.

- **Patch parsing, matching, and generation.** `Tidepool.Patch` is a production
  consumer after all: generated `planUpdate` calls `Patch.genPatch` and
  `Patch.renderPatch`, and the patch quasiquoter/runtime exposes parse, apply,
  invert, and inspection. Its runtime implementation converts `Text` to linked
  `String` lines. Myers uses an association-list frontier plus repeated list
  indexing, so lookup and line access add work beyond the stated `O(ND)` search;
  hunk application also scans candidate windows with list `take`/`drop`.
  Measure real `planUpdate` file sizes, edit distances, allocations, and native
  bytes. If material, use one authenticated Rust-backed patch boundary with
  indexed line spans and vector-backed Myers state, preserving the Haskell ADT,
  context-is-truth matching, ambiguity reporting, and round-trip properties.
- **Typed `FromJSON` traversal.** After native JSON materialization, Haskell
  still performs typeclass-directed object/array decoding. Keep this in Haskell
  unless profiles show it dominates: moving it requires a typed schema
  interpreter and risks duplicating the surface's generic/typeclass semantics.
- **Inspection rendering.** Workbench display remains a budgeted, resumable
  Haskell tree walk. Measure concatenation and allocation within the fixed
  display budgets; prefer removing repeated `Text` concatenation over creating
  a native renderer that would duplicate arbitrary `Display` behavior.
- **CSV/TSV helpers.** `Tidepool.Table` parses CSV rows through
  `Text -> String -> Text`. This is opt-in library work, not a per-turn runtime
  boundary. Consider byte-span parsing only if a real large-table workload
  makes it visible.
- **Compile-time quasiquoters.** JSON, format, patch, and Haskell-expression
  quasiquoter parsers run under GHC. Treat them as compiler measurements, not
  runtime intrinsic candidates; replacing them would duplicate existing parser
  authority unless compile profiles identify a specific dominant parser.
