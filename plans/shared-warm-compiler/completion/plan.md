# Complete the shared compiler service and remove the old cell path

Status: owner scaffolds and bounded repairs are being implemented and folded;
the service-only execution path is not complete or qualified. The controlling branch is
`integration/compiler-query-warm-final`. Read the
[current checkpoint review](review-de12466597.md) and
[next-wave assignments](next-wave.md) before dispatch. Use this branch's latest
published plan commit, record its OID, and inspect intervening code changes.

Implementation follows [the scaffolding and receiving assignments](execution.md),
including the final native, lifecycle and dataflow review corrections. Root owns
the hard receiving boundaries; Sol area owners complete and fold the work beneath
them. This execution plan supersedes older dispatch ordering.

This plan supersedes the implementation ordering and readiness claims in the
earlier campaign handoffs and `mid-plan-notes`. Existing detailed contracts remain
useful where they agree with [the locked decisions](contracts.md). The reviewed
goal is the **complete mandatory campaign**, with an earlier qualified Persona
candidate delivered as soon as its gate below passes. Optional deeper GHC import
environment reuse remains an experiment, not a release prerequisite.

[The initial review record](review.md) retains the pre-implementation audit;
the current review above supersedes its progress claims. "Production profile"
below means the recorded deployment profile and real prepared catalog/service
path (which may be `fast-dev`), not a new build profile named `production`.

[The code-quality plan](quality/plan.md) adds reviewed representation changes to
these same leaves: carry owner-issued relationships, remove duplicate derivation,
separate recorded facts from live admission, and make invalid lifecycle states
unavailable to callers. Read its receiving-owner/checkpoint table before dispatch.
It is mandatory receiving-boundary work, not an independent refactoring queue or
a reason to preserve the finite engine while building another abstraction layer.

## Current implementation and evidence

The published root checkpoint is `3c53bc547e` on
`integration/compiler-query-warm-final`. It is an intermediate integration,
not the release candidate. The compiler service, runtime, native issuer and
query owners are receiving further reviewed changes on their area branches.
The baseline already has persistent GHC infrastructure; this campaign replaces
repeated reconstruction and competing cell paths, not a process-per-cell design.

| Evidence | Exact boundary | Remaining gate |
| --- | --- | --- |
| Root `3c53bc547e`: affected native production/test consumers compile; real v26 first-stage native case 1/1 passes | Actual parser, captured source and native issuer, confirmed END and post-END refusal | Full actor history, declarations, multiple stages and real private operand |
| Weak association: real lost successful delivery and separately held completed-output controls pass | Installed observations do not own their runtime source/target; admitted outputs retain actual custody | Joined candidate abandonment and reload/force/recovery histories |
| Canonical source `bba012bfd3`: four runtime/actor consumers compile, 29/29 controls pass | Source authority independent of lexical/native resource roots; source gates, per-site histories and core lifetime cases | Joined actual native contribution, parked values and force recovery |
| Closed input origins `fb4a88f912` with preparation transport repair: consumers compile, focused origin/scope control 1/1 passes | Base World, prior stage and private operand correlate to their actual captured owner | Real private B over public A, parser END then new preparation, crossed runtime owner |
| Native authored custody `6827c71114`: consumers compile, 2/2 scope and persistent-snapshot controls pass | Same stage ArtifactView reaches the existing authored-declaration issuer; issuer and carrier scopes remain distinct | Ordered stage fold and cumulative runtime lexical publication |
| Queries `8b0920cab9` + `3b61732cec`: affected consumers and authored fixture compile | Nominal published binding, all five query operations, returned identities/provenance, ordinary file read and stale pin after publication | Execute the joined hosted fixture; compilation is not execution |

Exact commands, artifact hashes, selected/executed counts and logs are retained
under `/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/`, especially
`completion/engine/`, `completion/queries/` and
`post-de124-review/canonical-source-checkpoint.json`. The closed-origin handoff is
`/srv/swarm/evidence/inanna/world-input-origin-receiver-20261020/handoff.json`.
The authored native scope controls are under
`/srv/swarm/evidence/inanna/world-stage-native-scopes-final-20261020/`.
Leaf passes qualify their exact revisions and boundaries only. Frozen-bundle
qualification and a new Persona launch have not passed.

### Current receiving frontier

| Owner | Concrete result in progress | Required discrimination |
| --- | --- | --- |
| Root runtime + engine receiver | Opaque runtime operand and parser plan, real actor input extraction | Exact mounted input and binding lifetime survive END; wrong original or World refuses before claims/hooks; no public World mutation |
| Haskell + compiler request | Owner-rendered stage and exact prior-stage prefix, same retry owner | No raw caller source/plan pairing; later-stage type failure runs no early effect; declarations/imports/expressions remain supported |
| Engine + native assembly | Ordered actual native outputs and authentic cumulative lexical projection | Stage Lib is not relabelled as a facade; actual declaration join and runtime delta advance the World without finite compile-view reconstruction |
| Lifecycle + source owner | Receive canonical source token throughout publication, parked values and cleanup | Automatic resource release preserves source authority; explicit cutoff and stale target refuse independently |
| Native selection + runtime provenance | Actual selected support acquisition follows the same winning source-choice branch | Merely available equal-interface code adds no source obligation; actually selected supports retain their exact owners; passive recovery is not live authority |
| Lifecycle retirement/recovery | Unreserved publisher result crosses the real RunControl owner, then reservation/cutoff | Lost delivery before/after reserve, actual A/B registration and acknowledgements, durable B import, no unauthorized replay |
| Query + verification | Execute semantic all-five hosted program and real private-input controls | Correct returned evidence, no effect authority inferred from row fit, unchanged public state after read-only query |
| Engine qualification | Full syntax/history gate, legacy-path deletion audit, then frozen bundle | One production cell path, real actor execution and exact bundle metrics |

The native support owner keeps an explicit residual qualification step only when
an existing interface-only role becomes a selected Original. A fresh-owner or
canonical-only insertion must not rescan all retained native rows. Preserve exact
historical dependencies and genuine conflict refusals before optimizing this
remaining transition.

Compiler input availability and executable demand are separate facts. A protected
helper RHS may require an input's type even when the selected runtime root does
not reach it. Retain the compiler's actual reference inventory and derive runtime
requirements from actual emitted root/dependency evidence. Across stages, preserve
the original producing/consuming witness; do not reconstruct it from names or
stamp an earlier capture as a later one.

The 32-case historical sweep still needs explicit dispositions. A stale fixture
must be replaced by coverage through its real owning API, not weakened until it
passes. Existing exact historical dependency refusals, source-defined TYPE
controls, native custody properties and cold restoration remain required receiving
controls. The current first-stage pass does not supersede them.

Resume a completed worker explicitly for its next wave. Every area delivers a
reviewed consolidated checkpoint; the root receives and pushes those checkpoints
rather than leaving accepted work only on leaf branches.

## Delivery tree and ownership

Use one campaign coordinator, three area integrators and bounded implementation
leaves. DeepSeek workers can fill these roles; the contracts do not depend on
model names. Independent review is required for exact candidates. Reuse a worker
with relevant context for repair, and another owner for review. Keep no more than
twelve active nodes initially; expand only for an independent ready assignment.

| Area integrator | Leaf assignment | Owned result and dependency |
|---|---|---|
| Engine integration | [Native selection](native-selection/plan.md) | Exact native resolution, first publication and current projection. Supplies the authority contract used by service output issuance. |
| Engine integration | [Service cells](service-cells/plan.md) | Full supported cell preparation, existing executable output issuer, incremental successor/realization acquisition. Can develop against the agreed native contract in parallel. |
| Engine integration | [Runtime publication](runtime-publication/plan.md) | Sole actor entry point, private item adoption, atomic final publication and deletion of finite preparation. Depends on executable service output; starts consumer compilation immediately. |
| Lifecycle integration | [Lifecycle](lifecycle/plan.md), source/custody leaf | Original A/target B source ownership, preview admission, root/receiver cleanup and late completion. Supplies shared source-admission changes before runtime cutover. |
| Lifecycle integration | [Lifecycle](lifecycle/plan.md), retirement/recovery leaf | Default/force reload, fence/capture/drain, exact recovery and lost-waiter settlement. Uses existing generation and operation owners. |
| Verification integration | [Production tests](tests/plan.md) | Port valuable histories to real issuers; remove fixture authority factories; close every current red-case disposition. Build the first joined history before expanding the suite. |
| Verification integration | [Queries and docs](queries/plan.md) | Execute meaningful all-verb composition, consistent observation/fencing, useful authored examples and obsolete query-surface removal. |
| Verification integration | [Qualification and timing](qualification/plan.md) | Early Persona gate, exact frozen bundle, lifecycle matrix, measurements, final map and branch reconciliation. May be the same worker as tests until there is independent work. |

Each leaf owns its listed files in its isolated implementation checkout and folds
reviewed work into its area's branch. The campaign coordinator folds areas into
this integration branch repeatedly. Shared runtime admission/publication files
have the runtime-publication owner: lifecycle workers send focused shared-boundary
commits for that owner to receive before continuing, rather than independently
rewriting the same constructors. Native-selection owns selection policy; the
service leaf consumes its API and does not add a second resolver.

For the current wave, the engine integrator owns the complete R1/S1/actor
behavior across these files. It receives bounded leaf edits but must not leave
bootstrap, executable sealing or the caller patch unowned because a file belongs
to another leaf. Follow `next-wave.md` for the ready assignments and order.

### First dispatch and successive folds

The coordinator starts from this branch's latest published plan commit in an
isolated checkout, records its exact code base and refreshes GitHub refs without
upgrading dependencies. Suggested branches are `completion/warm-engine`,
`completion/warm-lifecycle` and `completion/warm-verification`, with leaf branches
under the corresponding area. Names are labels; recorded OIDs/owners establish
the actual dependency and review boundary. Provision only checkouts that need
build outputs, and retain evidence outside the source tree.

| First ready assignment | Deliver before expanding | Next fold |
| --- | --- | --- |
| Native N0 and service S1, under engine integration | Existing output/selection constructor contracts, exact lost-binding evidence and paired consumer compilation. Name owners of shared functions before edits. | Native N1/N2 and full service producer; join actual two-cell path with runtime. |
| Runtime R1, under engine integration | Capture real published base/source A/target B, pair actual admission signatures with service and source/custody leaves. No stub authority or publicly supported second route. | Private settlement/final publication; all syntax; delete finite callers. |
| Lifecycle source/custody | Original A, Open-gate and same-custody contracts on existing owners; forward shared runtime edits. | Retirement/recovery leaf starts its retained attempt and endpoint controls against those contracts. |
| Tests, under verification integration | Red-case ledger, independent oracles, reusable real-producer cohort and first joined history. Planning test setup must not invent missing authority. | Port families as receiving owners land, run beyond first failing assertion, then owning suites. |
| Queries/docs | Current snapshot/pin semantics, compiled semantic example and precise consumer requests for runtime/service. | All-five public execution through the new route, then early Persona. |
| Qualification | Confirm retained baseline and bundle inputs, prepare commands and evidence layout; no full rebuild of an unchanged unready candidate. | Time the first real joined path; freeze G2 candidate; continue G3 matrix in parallel with live use. |

Area integrators review/fold small compatible commits and tell dependent owners
their received OIDs. Shared-file signature changes are serialized, while isolated
consumers, authored fixtures and independent investigations can proceed. A child
finishing a wave sends its parent the next ready assignment or exact blocker;
the parent reviews and dispatches it instead of leaving a completed leaf idle
with unmerged work. Consolidate an area's current accepted state before a model
or account handoff. No implementation wave is started merely by writing this plan.

## Sequence and gates

### G0: preserve and establish the contracts

1. Publish the clean joined checkpoint and this plan. The initial `github` remote
   is the public repository; `origin` is a local plan checkout. Do not confuse
   local `origin/main` with a fresh GitHub observation.
2. Record source and evidence for every retained parcel and dirty worktree. Keep
   the scratch diagnostics untouched. Root-E, custody, Batch B/C and reload are
   source material for reviewed forward repairs, not an automatic merge queue.
3. Adopt `contracts.md` and the receiving sections of `quality/plan.md`; compile
   shared signature changes and their actual consumers early. Read local
   contributor guides and the pinned build guide.
4. Establish the native exact-binding refusal and real world-cell execution
   controls. Diagnose A1c at the exact receiving source using existing bounded
   owner/choice traces. Do not broaden inventories to make it pass.

### G1: complete one real vertical path, then replace the entry point

The engine integrator joins native/output/runtime work incrementally. First prove
two real imported-module cells and exact successor publication. Immediately extend
to declarations, imports, expressions, mixed cells and source-defined retained
types. Reuse the existing parser, checked-item issuers, evaluator and publication
owner. The first successful integer example is a debugging milestone only.

Replace actor admission in place and delete the finite cell route, its enum arms,
getters, fallback selectors and test-only callers. Runtime native authority and
physical custody remain required; compiler-derived reconstruction does not.
Startup, renderer/host preparation and recovery must use the same compiler service
for their compiler work while retaining their genuine runtime purpose differences.
There is no release flag or compatibility shim that sends cells to the old route.

### G2: offer an early Persona candidate

Freeze a production-profile bundle from the new route as soon as all of these pass:

- Installed Persona source/spec, initial memory/context and tools are present;
  scripted hosted inference invokes the actual `haskell` tool and handlers.
- Ordinary bind, declaration, explicit/empty import, expression, mixed cell,
  source-defined retained type and a later reuse execute on the service path.
- All five discovery verbs return asserted semantic fields. The published
  structured example runs; later discovered behavior is actually invoked.
- Successor remains usable after predecessor realization eviction and removal
  of disposable operation output; immutable source/native custody remains owned.
- Stale publication, source revocation, cancellation/uncertain END and default
  reload refusal preserve the old public state and truthful effect receipts.
- The old cell path and authority-forging test setup are absent from these
  workflows. Actual operation/world/worker/output identities prove the route.
- A bounded real DeepSeek multi-turn smoke completes using this exact bundle.

Then launch a fresh Persona conversation, preserving its durable memory and authored
files, and provide the URL. Do not wait for the final performance matrix or all
force-retirement scenarios. If force is unfinished at this checkpoint, it must
explicitly refuse; full force/recovery remains required for G3. Any known defect in
the candidate's advertised ordinary workflow blocks this gate. No "likely works"
claim based on the legacy hosted pass.

### G3: finish the mandatory campaign

Complete force retirement, forks, cold recovery, worker replacement, exact
historical dependency and physical ownership histories. Close all 32 recorded
failures through repair or an explicit obsolete-test disposition whose valuable
contract is covered on the new engine. Do not ignore failures or weaken equality,
permission, lifetime or read-cost assertions to obtain a green suite.

Run affected native suites and compiler fixtures, then bundle-owned M1–M3 and the
authored Persona histories on one exact final bundle. Measure the real workloads
as specified in qualification. Update `docs/compiler-work-map.md` to describe the
shipped route and remaining measured costs, with no historical claim promoted to
current evidence.

## Coordination, review and completion

- Every task records base/head, owned paths, dependency commits, exact commands,
  selected/executed counts, terminal status and evidence location. No helper-only
  implementation is complete without its production consumer.
- Review the complete base-to-tip delta and its refusal/cleanup paths. Each review
  has one primary invariant and adjacent checks for surviving bypasses, repeated
  work and fixture coupling. Review approval and passing counts are separate.
- Integrate compatible reviewed commits promptly; keep area branches rebased on
  received contracts. An author must not silently absorb a conflicting authority
  policy during rebase. Resolve it with the named owner and rerun joined controls.
- Children notify parents at proposal, blocked, review-ready and settled states;
  parents assign the next ready frontier. Use completion events, not busy polling
  or an unrelated task to fill a slot. Do not create native Codex goals.
- If a genuinely new architectural decision is necessary, retain its smallest
  counterexample and competing options for staff review. Continue independent
  work, then leave a clean consolidated handoff; do not guess or hide the problem
  with a fallback.
- Use admitted local-only pinned builds, actual RAM/cgroup/scratch headroom and
  bounded jobs. Preserve source, evidence and required products. Final storage
  analysis identifies reclaimable owned outputs; it does not authorize broad GC.
- Push integration checkpoints. Before final main integration, refresh the actual
  `github/main`, run the repository integration status report, reconcile any new
  changes and requalify the received code. Do not switch the interactive checkout.
  This audit found GitHub main `545d9950e5` was an ancestor of the joined branch;
  that observation must be refreshed. Main advancement is a final reviewed
  integration, not a claim that today's branch is already qualified.

Completion means one supported resident compiler route, migrated production API
tests, all mandatory lifecycle behavior, a matching qualified bundle, an available
Persona run, measured timing/work evidence and a reconciled published main. Optional
arm C and unrelated product requests stay explicitly separate.
