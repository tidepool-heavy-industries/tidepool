# Complete the shared compiler service and remove the old cell path

Status: reviewed implementation plan; implementation has not started under this
plan. The controlling source is branch `integration/compiler-query-warm-final`,
reviewed at `6f7ae09b3bdbcfd8952d9a513db91564ea1b5858`. Its code tip is
`92fa518105`; intervening changes are Markdown, including shipped guidance.
Use the latest published plan commit on this branch at worker kickoff, record its
OID, and inspect any intervening code changes before proceeding.

This plan supersedes the implementation ordering and readiness claims in the
earlier campaign handoffs and `mid-plan-notes`. Existing detailed contracts remain
useful where they agree with [the locked decisions](contracts.md). The reviewed
goal is the **complete mandatory campaign**, with an earlier qualified Persona
candidate delivered as soon as its gate below passes. Optional deeper GHC import
environment reuse remains an experiment, not a release prerequisite.

[The review record](review.md) distinguishes audited facts, corrected older
claims, remaining investigation and cross-plan findings. "Production profile"
below means the recorded deployment profile and real prepared catalog/service
path (which may be `fast-dev`), not a new build profile named `production`.

[The code-quality plan](quality/plan.md) adds reviewed representation changes to
these same leaves: carry owner-issued relationships, remove duplicate derivation,
separate recorded facts from live admission, and make invalid lifecycle states
unavailable to callers. Read its receiving-owner/checkpoint table before dispatch.
It is mandatory receiving-boundary work, not an independent refactoring queue or
a reason to preserve the finite engine while building another abstraction layer.

## What the review established

- The compiler-query handlers reach real GHC. A persistent compiler universe and
  bounded worker infrastructure already exist. Do not describe the baseline as a
  new GHC process for every cell.
- Ordinary actor cells still use finite admission, compiler-view reconstruction
  and all-value preparation. `admit_world_cell_program` has no production caller;
  world item settlement and final publication return explicit missing-capability
  refusals. `retainHeldWorldSuccessor` has no caller. Passing Persona successor
  tests therefore do not establish the required cell migration.
- World preparation currently excludes declarations/multiple inference segments,
  source-dependent retained value types and ordinary expression reservations.
  These are required completion work, not unsupported Persona profiles to retain.
- The recorded full toolchain run has 32 failures. Its 1054 passing cases and
  named hosted passes are historical evidence, not acceptance of this plan.
  Several proposed repairs weaken or bypass their oracles. Preserve the useful
  contracts while replacing those fixtures.
- The A1c structured-introspection probe fails during compilation of a later
  probe declaration. The initial `info` example reached its first display check.
  A missing exact native dependency is not proof that a module/interface is
  absent from all retained custody. Its exact lost selection remains to be traced.
- The join already has stronger Fresh/Bound/Consumed preview transitions than
  the stale reload follow-up. Original-source retention, final custody and
  generation retirement still have gaps. Do not merge that follow-up wholesale.

The source audit ran no new builds or tests. Retained evidence is under
`/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/`; the useful starting
records are `final-join/TOOLCHAIN-SWEEP.md`, `reviews/A1C-TIP-FINDING.md` and the
adversarial parcel reviews. Findings in a dirty instrumentation checkout are not
evidence about this joined source.

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
