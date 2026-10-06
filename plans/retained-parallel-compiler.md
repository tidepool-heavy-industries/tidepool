# Retained, incremental, parallel compiler delivery

## Accepted outcome

Retain completed compiler contexts and immutable products, compile only new or
changed work, and schedule independent work along the actual dependency graph.
This is a trusted development environment: boundaries preserve lexical meaning,
dependency identity, ownership and recovery, not security isolation between
actors. Completed private contexts may remain resident and share unchanged parts.

Performance gates, excluding provider inference and authored tool execution:

| Workload | Gate | Work invariant |
| --- | --- | --- |
| Distinct warm tiny cells | p95 < 1 second | No unchanged dependency frontend/finalization |
| Fresh process with prepared workspace | < 10 seconds to root ready | No driver/installer source compilation |
| Same-toolset child setup | p95 < 1 second | Shared installer/native images; fresh actor state |
| First preparation | Report separately | Required products prepared once; graph parallelism |

Include CLI preflight, admission and native compilation in startup accounting.
Measure warm cells while background preparation is running. First preparation
must not disappear outside the reported timeline. The completed delivery also
requires the real M2 worker/capture scenario, not only compiler benchmarks.

## Shared contracts and owners

### Retained compiler contexts

The existing resident compiler owner retains completed HPT/module graph views,
canonical interface/Core pairs, hydrated details, prepared products and recovery
results. Product validity includes exact owner/dependency identities, producer,
profile and compilation-affecting inputs; a request ID or actor incarnation must
not invalidate unrelated support.

Select persistent context views explicitly. A -> B -> A retains A while B is
active. Do not rebind mutable cells captured by A's deferred computations. EPS
and finder caches belong to their resolution context; name interning remains
worker-wide. Stable completed private contexts are valid reusable state.
Attempt-local targets, hooks, diagnostics, pending finalizations and failed
partial additions do not become unconditional shared facts. Bind retained
unfolding inputs to stable contexts rather than a repeatedly overwritten cell.

Retain compatible interpreter/linkable state. Classify actual BCO and foreign
object components; unload/relink changed supported home code and rotate only
for incompatible object replacement or unconfirmed recovery. No interpreter per
request. Retained code does not authorize skipping newly requested TH effects.

Fix stale same-named ModIfaceCache selection before broadening lifetime; keep
interface and linkable together. The existing transaction-issued capability
also owns parser defaults and scoped exact-interface/inspection operations.
Remove resident helper runGhc bootstraps; the one-shot transport boots the same
owner once. Memory pressure may evict unused acceleration without invalidating
original artifacts needed by captures.

### Artifact dataflow

Extend existing ArtifactInventory/ArtifactView and product owners. Private
carriers bind decoded values, original bytes, digests and dependency evidence.
Retain owned immutable materializations; requests reference them and produce
new outputs. Eliminate immediate write/readback, repeated finalized serialization
and duplicate decoding. Validate shared source evidence once per acquisition or
publication stage, keeping pre/post mutable-source observations distinct.
Admission validates immutable products once; activation selects retained owners.
Do not add another registry, replay authority, fixture format or compiler.

### Dependency execution

Use threaded GHC and its module scheduler for the surviving source DAG. Preserve
boot/TH ordering, module-local Sessions and existing deferred lexical barriers.
Restore scoped job flags in the resulting HscEnv, never an obsolete HPT.

For postload work, separate context acquisition, independent computation and
short deterministic incorporation. Sibling identities come from finalized Core
before lowering. Acquire exact site/type authority and lowering inputs through
their owner; retain typed Names/full module identities. Freeze reuse decisions
instead of making validity depend on completion order.

Completed module preparation immediately enables raw group projection; new
demands enable original recovery. Retain raw projection results, not premature
global unavailable classifications. Distinguish queued/running/available/failed
work, settle only after all completion-driven demand is closed, and preserve
recursive groups and module-level unavailability propagation. Package body
subsets have versioned demand sets; a permanent visited-owner bit is incorrect.
Replace FatIface's lock-around-loading with short map access and per-key shared
completion, including cancellation and failure. One bounded execution allowance
serves all stages; no nested all-core pools or whole-compiler mutex.

Issue the successful current-original inventory through CompilerProducts before
target closure. Target projection references those original groups instead of
also embedding their support definitions. Only successfully admitted executable
groups qualify; interfaces alone do not. Keep the current target entry/recursive
group inline, preserve package/literal closure, and certify the complete batch
before execution. Measure target plus demanded images, not target size alone.

### Prepared source and actor installation

Buck produces the fixed root entry from declared inputs through the production
artifact owner. Complete entries retain target/table/sites, exact groups and
owners, package closure and original source/compiler evidence. Portable test
fixtures or a deserialized digest cannot issue startup authority.

AgentSpec installers compile from frozen published source roots and canonical
ordered actual effect specialization. Remove implicit ActorCompileView notebook
state from installer construction; no ambient fallback or new parameter ABI.
Ordinary cells and captures keep their existing lexical semantics. Select trusted
nominal effect/scaffolding owners, not only rendered names or one root's revision.

The source owner coalesces identical preparation and retains complete entries
plus strong native-image ownership when declaring them ready. Keep ImageRegistry
weak; do not create an unbounded global cache. Execute installation per actor with
fresh heap, dispatcher, grants and lifetime. Losing one waiter does not cancel a
shared preparation. Reload prepares a frozen replacement and swaps under existing
source/installation fences; stale completion cannot replace current tools.

Add exomonad prepare for root/configured worker entries. Init prepares root first,
starts it, then prepares configured common worker specializations in background.
Check remains check-only; do not enumerate all effect subsets.

### Resource admission

Extend the existing daemon admission before ACCEPTED with typed workload class
and execution grant. Reserve warm foreground capacity. Preparation consumes the
remaining admitted CPU/memory, using a wide worker for shared work and additional
workers for independent specializations after common products publish. No worker
per actor/module and no second supervisor/jobserver.

Qualify capabilities/jobs through 2/4/8/16 and effective logical-CPU capacity;
compare physical-core/SMT configurations. Select the smallest passing allocation
within 5% of best latency, using remaining capacity for independent work. Bound
admission by actual enclosing cgroups, host headroom, build commitments and
measured aggregate peaks. RSS rotation is not live memory admission. Change
grants only at settled transaction boundaries. Background cannot borrow the
foreground reservation through a non-preemptible load.

## Application order

1. Integrate reviewed correctness repairs: stale interface selection, genuine
   imported-group/quotation fixtures, support/receiver oracles, diagnostic
   preservation, cleanup settlement and stack ownership.
2. Scaffold/review shared lifetime, product and execution-grant contracts.
3. Parallel component owners implement retained contexts/helpers; graph
   preparation/recovery; artifact ownership/current-original projection;
   prepared startup/reload; daemon admission; semantic properties/measurement.
   One compiler owner coordinates overlapping pipeline changes. Sol leads use
   Luna for bounded leaves; independent review checks exact candidates.
4. Integrate regularly on main. One owner schedules heavy builds/qualification
   from the provisioned checkout; source worktrees do not start Buck. Preserve
   other workers' changes and source-only evidence.
5. Freeze one matched bundle, run correctness/corpus/M2/performance gates and
   deploy a fresh browser trial. A coherent earlier build can support human
   tests; the complete delivery remains open until all gates are measured.

## Acceptance

Use real compiler producers/current typed fixtures, not hand-authored authority
or baked wire versions. Property suites belong in the repository.

- Same-slot A/new cell/B/rejected-or-cancelled B/A, including hidden instances,
  families, lazy interface demands, changed search order, source shadowing,
  retained originals after source removal and missing-interface recovery.
- TH compatible reuse/changed closure/foreign object branch; failed attempts
  preserve completed reusable work and truthful cleanup outcomes.
- Diamond/duplicate demand, legal boot cycles, native cycles, delayed whole-owner
  failure, package subset growth with reordered completion, cache cancellation.
  Compare serial and shuffled parallel semantic results and stable ownership.
- Independent reference graph/recomputation models; calibrate property generators
  using deliberate invalidation/scheduling defects, then remove mutations.
- Prepared root without source compilation, same-toolset code sharing with fresh
  mutable state, effect-order/reload/waiter cancellation controls.
- M2: A parks/B publishes/A resumes without erasing B; two children use a capture
  before parent completion and survive later parent failure. Fresh Sol root/Luna
  worker TUI exercise, messaging and cancellation in a new workspace.
- At least 20 distinct warm cells and child setups, five fresh-process prepared
  startups. Include background contention and actual task-overlap evidence.
- Retain source/artifact hashes, commands/counts/results, queue/service times,
  actual frontend/finalization/projection work, bytes read/written/decoded,
  native hits, CPU/allocation/GC and aggregate peaks. Use existing codegen detail
  and standard GHC profiling/eventlogs. Do not sum overlapping CPU phase counters.

Performance misses remain explicit open results. An identical request cache hit
does not substitute for genuinely new-cell acceptance.
