# Performance comparison and integrated release

Measure the coherent path after each accepted fold; qualify the exact integrated
bundle before declaring delivery. Follow the existing
[profiling](../../../docs/compiler-profiling.md) and
[frozen bundle](../../../build/package/README.md) owners. This plan adds no
latency threshold or automatic release/activation action.

## Matched baseline and workloads

Freeze baseline and candidate with the same selected profile, workload roster,
shared workspace/source contract and declared runtime resources. Retain exact
compiler/Harness identities, bundle/descriptor hashes, commands, worker counts,
foreground/preparation allowances, process concurrency, trace profile and
machine load observations. If a semantic fix changes required behavior, rerun
that baseline or state explicitly why a particular before/after case is not
behaviorally comparable.

Use `minimal` tracing for primary paired latency runs and the frozen owner's
applied capture receipts. Use bounded detailed captures for phase/counter
attribution and sensitivity; report their observer overhead separately. An old
record with no applied receipt is unverified, not implicitly full capture.
Start with a small calibration, then interleave paired baseline/candidate
repetitions. Predeclare repetition count, outlier treatment and cold/hot setup;
retain all attempts and explain failures rather than discarding slow results.
Use enough samples for the percentile claimed; otherwise report individual
observations and median/range without inventing p95 confidence. After baseline
calibration and before an experimental promotion sweep, record the measurable
useful-benefit criterion, acceptable variance and memory/scratch/pressure limits
with the qualification and area owners. Choose them for the actual workload and
resources, then keep them fixed for the decision; do not choose a favorable
metric or threshold after seeing candidate results. No forced 10× criterion is
introduced here.

| Workload | Meaning of cold/hot and required scale axes |
| --- | --- |
| Startup/repeat launch | Separate first toolchain realization, workspace preparation, prepared root readiness, fresh host startup and shutdown. Prepare once/repeat launch does not hide its initial preparation |
| Warm distinct tiny cells | Fixed new source/body size and dependency demand; increasing selected scope, irrelevant retained history and sibling count independently |
| Same world and A→B→A | Exact-world hit, changed world, compatible/incompatible original/TH closures; same actor with different worlds and different actors with the same world |
| Fork/capture/drop | Shared, overlapping and disjoint roots; fixed small worker pool with increasing logical actors; release parent/siblings while one value remains |
| Cold/spill/restart | Service reference miss, realized-world eviction, worker replacement/epoch rotation and authenticated cold recovery, without deleting shared caches |
| Reload/adoption | No-op, changed candidate, default blocked, forced confirmed and partial retirement; include preparation, fence/drain, publication and receipt |
| Packaged Persona/coding | Complete existing named histories with their assertions, actual calls, deployment selection and cleanup; do not replace them with a synthetic kernel-only benchmark |

For scaling, choose at least three increasing sizes after calibration (for
example geometric N and actor counts) within observed memory/disk limits. Hold
changed facts D, actual affected set A, requested operation and new output size
constant where the hypothesis requires that. Then vary D/A separately, including
one change whose legitimate invalidation is broad. Record actual generated
facts/edges/bytes; input parameter names alone do not prove the intended shape.
Use the same scaled histories and independent oracle on both implementations.
For query sweeps, hold returned rows/bytes and inspected expression size constant
as scope grows. Report full export enumeration and growing signature/type output
as separate O(output) observations; do not hide their legitimate work in a hit
counter or require them to be constant-time.

## Structural observations and acceptance

Count at existing owning operations; do not add graph census traversals solely
to calculate a timer counter. Keep test-only/opt-in diagnostics bounded and
explicitly report unavailable counters.

| Boundary | Counters and falsifying observation |
| --- | --- |
| Host admission/P1 | Captured/selected/custody facts; support merges; cumulative role/native-choice visits; lexical/template copies/validation; current-view/hash bytes. Hot unchanged roots should not replay history; growth moved before P1 is still growth |
| Continuation/P6 | Owner-census visits, group authentication, import predicates/reverse affected edges, per-item capture and prefix publications. Newly invalidated contextual checks must run; unchanged immutable facts should not be rebuilt for each output |
| Transport/physical inputs | Complete metadata encode/decode rows/bytes, baseline interface writes/hash/readbacks, lease/part visits, FD enumeration and sideband descriptor exports, acquired input count and registration/realization hits/misses. Intercept `ExactCompilationRequest::input_transport_files` and its production encoder/export consumers: a tiny semantic wire and zero hydration do not prove the baseline was not walked/exported |
| GHC activation/frontend | `initialHomes`/HPT rows, generated import/source bytes, interface loads/decodes, reader/instance/default/orphan/family work, new-body typecheck/finalization, products and TH/code loads. Root lookup alone cannot prove frontend incrementality |
| Runtime/publication | Fresh installed targets/groups, native image reuse, snapshot/JIT/evaluation/publication work, effects per fixed intent and admitted/settled resources. Correct execution with stale rendering/type metadata is failure |
| Retention | Unique facts versus references; persistent nodes/edges; actual environment owners/retainers; selected slice versus backing arena/TempDir bytes; worker/interpreter mappings and last-owner release |

The accepted structural claim names its frontier. For hot host composition and
reference transport, fixed D/A should avoid N historical support folds,
whole-baseline encoding and copied-baseline bytes on a genuine realization hit.
A matching Resident hit must borrow the retained exact physical-input owner
without enumerating/exporting baseline parts; miss/delta acquisition exports only
its required parts. Measure sideband descriptors independently of wire bytes.
Report any unavoidable enumeration demanded by the actual request. A selected
cold reconstruction may inspect the full scope; it must remain correct and have
a separate cost. Affected-dependency processing may also be broad when semantics
actually change. Do not equate pointer reuse, token hits or compact wire bytes
with cheap decoding, bounded retained heap or eliminated validation.

Fit/explain work counts before latency exponents. Compare counts across N and
show which repeated term disappeared, not just one faster timing. Report wall,
CPU/allocation, IO, peak/retained memory and storage together. If a claimed delta
operation still walks all history or retains complete prior environments per
snapshot, mark that claim unqualified even when the short workload gets faster.
A large end-to-end speedup remains an ambition, not a fabricated passing gate.
Use current measured critical-path fractions for Amdahl sensitivity; historical
`20d7`/B108 source and workload totals cannot supply those fractions.

## Interval accounting and host/GHC/startup attribution

Maintain one whole-test monotonic wall ledger with sequential phase boundaries,
startup/preparation, shutdown and an explicit unpartitioned remainder. Retain
host NEW/CLOSE ancestry, physical request/service IDs and completed worker spans.
For disjoint sequential phases, verify their sum plus remainder equals whole
wall within timestamp precision. For overlapping intervals use unions/critical
path; never add worker service time to its enclosing host wait/admit span or a
child timer to its parent. Concurrent children are one joined workload interval,
not the sum of both call walls. Report unmatched/duplicate joins and omitted or
truncated detail; buffered event arrival order is not execution order.

Separate at least: queue/admission wait; pre-P1 runtime work; offer; physical
request service/acquisition; host output admission; declaration projection;
outer END; contextual seal; runtime install/snapshot/JIT; evaluation; publication;
and startup/cleanup. Service duration includes more than GHC typechecking.
Use worker-local resource spans to attribute GHC setup, dependency loading,
checked typecheck and products where observed; an absent detailed span is
unknown. RTS allocation/GC counters are not retained heap or native RSS;
overlapping deltas must not be summed. Current-thread CPU omits other threads;
process-tree/cgroup peaks can include reused services or unrelated work.
Retain the actual scope of each observation.

Test measurement sensitivity before using zero to claim no work: known
preparation must produce a matched submission/service, and a controlled changed
input must affect its owning counter/phase. The frozen owner's named workload
roster, not the events that happened to survive, determines required coverage.
Behavioral completion and valid timing are independent assessments.

## Frontend experiment go/no-go

Join [the worker owner's experiment](../warm-workers/03-import-context-experiment.md):
current warm generated imports; persistent exact HPT with unchanged normal-module
entry; complete checked import-context or maintained scope-interface alternative.
Use the same actual ordinary-module path and fresh target identities. Full
`TcGblEnv`/AST retention per actor is not the default solution.

Acceptance requires parity against the declared normal GHC/module baseline:
values/types and nominal identity; aliases/qualification/hiding/shadowing;
fixities/defaulting; import availability/provenance/usages; class and family
instances, orphan visibility and consistency refusals; source/boot/module/unit
relationships; plugins and TH dependency/code changes; exact source/original
and prepared/native products; diagnostics/refusals; cancellation and future
requests after failed attempts. Preserve all exact runtime selection checks.
GHC is the independent language oracle; a helper shared with the experimental
import seed cannot independently establish these semantics.

Hold tiny new source constant while increasing unchanged scope; count actual
HPT/import/reader work and measure retained owners. A frontend arm proceeds only
if it preserves that contract and removes the claimed repeated term with bounded
retention and a measured useful benefit under admitted resources. Otherwise
stop that arm, delete unaccepted bypass surface, retain its counterexample and
publish the accepted lower-level gains with an explicit remaining O(N) module
frontend floor. Do not silently migrate to GHCi declaration execution or call an
HPT-root lookup a checked-import result.

## Integrated frozen qualification and resource limits

Use the current bundle owner to build/freeze at final absolute paths and verify
source/submodule/profile/compiler/assets before execution. Rebuild/refreeze after
source or declared fixture changes; never patch a frozen descriptor or substitute
compiler/libtest paths. Discover selected exact cases from each sealed roster.
A typical existing workload invocation is:

```sh
python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" run "$DESCRIPTOR" \
  --cohort harness-performance-persona --output "$PERSONA_EVIDENCE" --jobs 1 \
  --compiler-mode owned-resident --compiler-workers 2 \
  --foreground-jobs 1 --preparation-jobs 1 --trace-profile minimal \
  --delegated-service --service-slice "$ADMITTED_USER_SLICE"
```

Run inside the already admitted boundary specified by
[the resource plan](01-baselines-and-oracles.md#resource-admission-before-execution).
Reobserve headroom before every expensive selection and inspect actual compiler
and Buck descendants. Keep test processes at the cohort's limit (normally one
for these histories); compiler workers and job grants do not authorize extra
processes. The offline coding cohort requires its declared two-worker, 1/1
contract. Scaling logical actor count does not increase compiler pool size or
retention allowance automatically. No cache clearing, memory-limit relaxation
or broad socket permission change is an acceptable measurement setup.

Run the complete affected Persona/startup/reload, notebook, concurrent/captured
child and coding rosters from [joined histories](02-joined-histories.md), plus
unchanged release gates required by the parent delivery and any directly touched
fixture/corpus contracts. Compiler translation/serialization changes require
`just fixtures-check` through its existing independent GHC corpus producer.
Full `just verify` belongs to the agreed integration boundary, not every spot
check. Source review, formatting and `git diff --check` remain necessary and
separate from compilation/execution.

Use `qualification.read_measurement(report_path)` and the bundled reporter's
`require_claim` for the exact selected workload, as documented by the package
owner. `behavioral_completed`, `behavior_selection_completed`, `workload_timing`
and `qualified_frozen_timing` are different claims; exit 0 alone establishes none
of the latter measurement claims. Retain known subtotals and unknown operands.

Qualification owns the compact update of `docs/compiler-work-map.md` after
each accepted area fold and reconciles it again before the final handoff. Each
area owner verifies its joined issuer/consumer paths and current contracts. Bind
the map to accepted source; show selection versus custody, source-free protocol
and request/END/publication owners, counters and phase ancestry, including pre-P1
admission and declaration projection. State current remaining cost and distinguish
measured work from unmeasured claims. Replace obsolete source/status claims
rather than append checkpoint history. This updates the existing map, not another
framework. Coordinate its single editor through the qualification lead.

The final handoff names exact accepted source and artifacts, all executed
counts/statuses, semantic migration decisions, measured structural/latency/
retention changes, unresolved regressions and the commands/evidence needed to
resolve them. Report deployment separately. A release is not qualified by a
smaller defect list, a compiled candidate or a predicted speedup.
