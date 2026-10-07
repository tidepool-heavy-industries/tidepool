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

Include CLI preflight, daemon readiness, admission and native compilation in
startup accounting.
Measure warm cells while background preparation is running. First preparation
must not disappear outside the reported timeline. The completed delivery also
requires all seven frozen M2 scenarios, M1 and the prepared corpus. A server
qualified through those correctness gates can run while performance qualification
finishes; an accessible trial does not complete the performance gates.

## Shared contracts and owners

### Current delivery sequence

The next release separates prepared-worker correctness from the one-pass
frontend change. The accepted frozen source at `76135f71a9` passes seven M2
cases, M1 and the catalog gate. A later real prepared-child trial exposed an
additional preparation-coverage defect: configuration selected the research
authority ceiling, while the public Haskell research profile omitted `Sleep`.
The different installer recipe legitimately took the unlisted-profile compile
path and re-executed its quoter. The original-selection loader was not the
source of that divergence.

1. Generate public Haskell effect aliases and Rust profile metadata from the
   existing protocol schema. Prepare concrete public profiles, keeping authority
   ceilings separate and always preparing the root's actual descriptor row.
   Preserve effect order and the support projection. Existing configuration
   role names remain supported; inherited preparation remains invalid.
2. Make prepared coverage an authoritative inventory of configured profile,
   requested and supported rows, installer recipe and original. Derive lookup
   maps from it. Refuse missing promised coverage and old incomplete manifests;
   require re-preparation rather than infer evidence. Preserve absence-only
   fresh compilation for genuinely unlisted profiles. Typed acquisition records
   distinguish deployment originals, new run originals, existing run originals
   and unretained compilation independently of singleflight disposition.
3. First run one actual prepared-child regression: prepare against input 41,
   change the external input to 42, then require the public research child to
   install and execute original 41 without installer compilation or quotation.
   The observer must first detect known preparation requests. Then run twenty
   sequential children through the same fixture with distinct installation
   scopes, exact typed original selections and parent replies. This is one
   additional mandatory frozen cohort; the one-child diagnostic does not add a
   duplicate mandatory cold run. Report setup P95 separately from correctness.
4. Refresh the retained catalog sources and freeze one matched bundle. Execute
   the affected component checks, the prepared-child cohort, unchanged M2/M1
   and catalog gates. Drive a new live Sol 6.1 root with Luna children and a
   grandchild through a small TUI project, using ordinary Haskell, commands,
   yield, typed replies and commits. Keep the qualified root available for
   human testing. A live failure must produce a deterministic component or
   component-history regression before its repair is accepted.
5. Independently implement the one-pass frontend below, then join and qualify
   it as a second coherent compiler change. Its source-only intermediates do
   not change the released compiler contract.

The one-pass compiler uses a complete ordered plan sealed to the existing
request reservation and source identity. One ordinary GHC frontend issues
opaque typed item descriptors containing actual Ids, their original types,
Name-keyed fixities, capture origins and predecessor evidence. The Session
owner batches thin value interfaces and hydrates their actual global Ids;
Core substitution consumes those Ids rather than reconstructing identities
from printed types or runtime variable IDs. Simplification and finalization
run once for the segment, producing multiple native entries with canonical
original group identities. Declaration segments must also lose their duplicate
successful check/produce frontend.

Normal GHC generalization is the language contract. Genuine generalized lets
and rank-N values survive; unresolved action results require a concrete
same-cell use or annotation. No pre-zonk hook or additional generalization
carrier is planned. Independently retained items keep their existing boundary:
cross-item existential type/dictionary/coercion evidence that cannot be
materialized is refused before execution; nested scopes inside one item work.
There is no hidden continuation or capture ABI extension.

Bare expressions use their actual typed occurrence from that same frontend.
Pure expressions retain a lazy unit thunk; actions with the exact expected
effect row run once and retain a thunk over the result. Wrong effect rows are
refused. Existing type-based publication strictness for ordinary bound values
is unchanged; recording capture origin does not authorize making scalar
publication lazy. Stage all fallible interface and projection work before
public installation, preserving cancellation and retry cleanup.

Native tests must cover ordered-root substitution, same-typed item swaps,
future captures, genuine sigma values, defaulting/MR/NoMR, pattern failure,
nested versus cross-item GADT evidence, observation laziness and effect-once
behavior. Request-history tests cover failed staging, cancellation and retry.
For uncached 1/4/8-item segments require one successful authored frontend,
the expected entry count and zero frontend work during execution; count
dependency frontends and legitimate failed instance attempts separately.
Measure wall time after these work-count invariants pass.

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

Whole-request equality is not the reuse boundary. A worker with the same pinned
producer and base compiler configuration retains versions by full module identity
and validated source, dependency, retained-binding and executable inputs. Adding
an unrelated binding or changing the selected view must not evict unchanged
support. Import validity includes instance, family and orphan visibility, not
only positively referenced term names. Each request activates its selected
environment without turning the retained version inventory into lexical scope.
Store completed versions once; do not append a full context snapshot or aggregate
every historical interface cache on each request.

Index versions by their known source/dependency inputs before validating actual
interface usages. Select recovery entries by owner rather than scanning each
historical context. Keep one package finder owner for the fixed package universe;
an attempt must not add another fallback layer to the previous attempt's finder.
Interpreter invalidation traverses reverse dependencies of replaced owners.

Retain completed lowering and exact-original recovery alongside interfaces.
Native, checked and presentation purposes must not repeatedly lower or recover
the same valid owner. Growing recovery demand prepares only newly demanded work.
Authority-bearing prepared code needs an opaque witness of the nominal and sibling
definitions actually consumed, including relevant negative lookups. A fresh
request ID cannot by itself invalidate that code. Issue the witness from the same
resolved values used by elaboration and revalidate it before reuse. Package
recovery may prepare canonical private-dependency components together when GHC
requires their local binder scope; retain stable original identities and report
the extra groups prepared rather than claiming binder-level demand granularity.
Interpreter retention distinguishes an inactive compatible module from a replaced
executable version; bytecode containing remote pointers belongs to its interpreter
epoch. Rotation preserves pure compiler products but cannot reuse stale remote
pointers. Reissue such bytecode from retained finalized Core when required.
An unsuccessful linker operation may mutate the interpreter before its registry
rolls back. Registry contents alone cannot prove recovery; uncertain executable
mutation retires the interpreter epoch while retaining completed pure products.

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

Complete compiler inventories require an explicit 4 GiB raw-input policy and one
shared 4 GiB decode-work budget across the inventory. Keep the independent
16 MiB standalone-program, 64 MiB module/group and 32 MiB durable-record policies
at their own readers. Decode work is an aggregate traversal/copy bound, not a
memory reservation. A complete catalog or original-entry inventory cannot inherit
the optional cache's 128-candidate cap or silently crop owners. Optional cache
overflow withholds excess acceleration while preserving the valid closed subset;
it must not weaken evidence validation or permit an incomplete exact acquisition.

Transfer a newly produced original once. If the receiving artifact owner already
retains that exact executable version, use the existing exact/candidate reference
path rather than emitting its complete payload again. Keep reusable products
distinct from a view's selected lexical or type-only authority. Preserve encoded
immutable group bytes and return the writer's bytes directly to certification;
do not immediately reread the file it just wrote. Descendant materializations own
only their additions and retain parents, with iterative shared-ancestor traversal.

Delete the obsolete implicit expression-display compilation. The current checker
does not request automatic rendering, but the planned-cell path still compiles an
opaque placeholder after every expression. Remove its unused programs, receipts,
extra generations and proof fields together. Native expression capture and effects
still execute once; explicit authored `display` and `expand` remain ordinary
effects, with their existing pagination and continuation semantics.

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

Exomonad prepare prepares the root first and the explicitly configured common
worker entries. Qualified init requires that completed deployment and creates a
fresh run; selected missing or invalid entries refuse without source fallback.
Configured worker preparation uses its actual ordered supported/granted effect
row. Uncovered roles retain explicit owned on-demand compilation. Check remains
check-only; do not enumerate all effect subsets.

Prepared workspace and installer artifacts retain an immutable source deployment
independent of live run identity. Multiple fresh processes/runs may select the
same completed artifact with fresh actor state. A source-only recipe key cannot
stand in for an exact completed TH output or bypass replay eligibility. Carry
source-owner manifests through ready lookup instead of rereading immutable roots
for every actor. Launch environment assembly consumes the selections already
verified during that acquisition; independent acquisitions still verify inputs.

Compiler purpose selects retained-generation policy through typed admission.
Pure activation previews observe only their mounted input and reject explicit
retained heap generations; they do not inherit heap demands from native support
groups. Executable requests preserve certified retained-generation demands.
Interface/native support evidence and live runtime-value authority remain separate.

Treat a proposed data-only parcel lifetime change as a hypothesis. First retain
a maintained test against a recorded source revision through the real typed
request, suspension, capture and retirement owners, with independent
settlement/lifetime observations.
Do not change production ownership because a data-only representation appears to
contain no executable work.

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

The central owner coordinates source capture and heavy builds. Independent native
cases and performance work may run concurrently when actual combined process
peaks, enclosing cgroups and host availability support them. Preserve roughly
20 GiB host headroom and interactive access; build, user and Nix limits do not add
up to physical memory capacity. A fixed single-heavy-lane rule is not acceptance
evidence, and additional compiler heaps need measured admission.

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
5. Freeze one matched bundle and execute the catalog gate, all seven descriptor-
   owned M2 cases, M1 and the full prepared corpus. Restore the six previously
   accepted M2 scenarios before integrating the held scope-input-fact reuse
   optimization. Qualify that optimization through its affected semantic and
   measurement gates after the hold clears.
6. From the correctness-qualified bundle, start a new workspace and a fresh
   Tailscale-accessible custom TUI on port 8082 with a Sol 6.1 root and Luna
   children/grandchildren. Retain old servers until this run and browser smoke
   succeed. Performance qualification proceeds concurrently when admitted; the
   complete delivery remains open until all required outcomes are measured.

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
- Run the native 50-distinct-cell workload with genuinely ordinary
  `TIDEPOOL_TIMING=0` and instrumented `TIDEPOOL_TIMING=1` conditions on the same
  matched closure. Prove the ordinary run emits no timing instrumentation.
  Report first cold preparation and A,A,B,A separately, with actual reused work
  and invalidation rather than identical-request hits.
- Execute at least 20 same-host prepared child setups and five fresh-process
  prepared startups. Prove shared code/native images, fresh actor state and zero
  driver/installer source compilation. Include real concurrent background
  preparation and admitted foreground reservation, queue/service intervals and
  aggregate memory evidence; an idle daemon is not contention.
- Compare CPU allocations on representative native source and postload graphs,
  not only tiny-cell smoke tests. Retain actual task overlap and the admitted
  physical-core/SMT choices alongside the 2/4/8/16/effective-CPU sweep.
- Retain source/artifact hashes, commands/counts/results, queue/service times,
  actual frontend/finalization/projection work, bytes read/written/decoded,
  native hits, CPU/allocation/GC and aggregate peaks. Use existing codegen detail
  and standard GHC profiling/eventlogs. Do not sum overlapping CPU phase counters.
- Emit reuse decisions at the stage that performs or avoids work: frontend,
  interface, finalized Core, prepared body/site authority, original recovery,
  raw projection, artifact reference/transfer and native image. Correlate them
  to the actual request/worker and owner version, retain concrete miss reasons,
  and mark observed stage completion. Missing instrumentation is unknown, not
  zero work. Calibrate counters against deliberately disabled reuse, and report
  distinct-cell, binding-growth and A/B/A behavior separately.

Performance misses remain explicit open results. An identical request cache hit
does not substitute for genuinely new-cell acceptance.
