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

- The compiler-query handlers reach real GHC. A persistent compiler universe and
  bounded worker infrastructure already exist. Do not describe the baseline as a
  new GHC process for every cell.
- The engine area's candidate now connects actor preparation to source-bound
  World capture, off-checkout acquisition, matched settlement and installation.
  Native World outputs can be sealed through the existing issuing owners. The
  actual actor history still fails during compiler preparation; component
  assembly and consumer compilation do not establish a working hosted notebook.
  The compiled area checkpoint `5d5c15b737` is now consolidated on the root
  integration branch. This is an intermediate source checkpoint, not acceptance
  of its known lifetime and authored-cell failures.
- World preparation still excludes declarations and multiple inference segments.
  The real actor history does not yet execute its first authored cell. Actual
  source-defined TYPE input, lexical exports and predecessor-free cold
  restoration have separate passing producer controls. Full-language completion
  reuses the existing admission and ordered stage fold; paired Haskell/Rust item
  variants must distinguish executable, declaration and prologue outputs.
- The 32-case sweep has a first-failure triage, not completed dispositions.
  The repaired residency fixture enters real source admission. Native completion
  previously stopped it before measurements; the owner-derived current-product
  scaffold now allows the bounded-residency and original-source gate controls
  to execute successfully. Broader collection histories remain receiving work.
  Q10's earlier four passes still concern synthetic observer histories.
- Native current membership comes from its actual certified source row.
  Integrated repairs preserve original publication custody separately from
  current compiler roles, traverse exact issued historical dependencies, and
  retain distinct owned payloads without selecting them. Both planners retain
  genuine ambiguity refusals. The test-only alternative historical issuer is
  deleted. The genuine hosted A1c target now declares the compiler-service
  deployment; its execution awaits the joined engine/Haskell producer.
- The opaque `SettledCellProgram`, declaration/host cancellation checkpoints and
  same-bundle refusal controls are integrated. A paired positive/negative
  compilation contract rejects constructing an executable bundle outside its
  settlement owner. This does not establish the whole hosted notebook history.
- Installed World associations still strongly retain the runtime leases they
  describe. A successful private birth whose receiver disappears, and an
  unpublished ephemeral reload candidate, can therefore retain themselves.
  The runtime owner is replacing both original-source and target association
  references with exact weak observations. Actual admitted/completed operations,
  generations and executions retain strong custody. Abandonment and held-output
  controls must pass through the real producer before accepting this repair.
- The core weak-association repair now has a real lost-successful-delivery pass
  at `a7c8fe08df`, plus a separately executed completed-output hold control.
  Prospective B constructors compile against the split at `4d6019aa34`.
  Their joined reception remains distinct from those component results.
  Per-site installed provenance exposed a separate whole-scope cycle; its
  [canonical source-token repair](contracts/SOURCE-AUTHORITY-LIFETIME.md) is
  accepted and assigned to the runtime/lifecycle/native receiving owners.
- Query pins now share one observed compiler snapshot across verbs. Full
  lexical queries still need the active effect row and exact input/request type
  witnesses carried from their existing issuers; names or displayed types are
  not substitutes. Recovered lexical facades must retain the exact issuing
  artifact, even when another artifact has equal interface bytes.
- The original-generation cutoff and retirement inventory are integrated.
  Reservation-owned A fencing and B-before-A refusal settlement have focused
  execution evidence. Linked publication, actual endpoint cutover, frozen B's
  World and durable force/recovery remain lifecycle work.
- Successor transport retention and cold restoration now have a discriminating
  production-reader control at `29ce1590e9`: both two- and eight-successor
  histories release predecessor transport and restore the same current product.
  This result does not qualify subsequent combined changes. Durable process
  recovery of a prospective reload World is separate work: live Arcs, FDs and
  temporary directories are not a durable candidate.
- A newer TYPE-only projection exposed an actual regression: applying it to
  general `support_for_owners` discarded passive historical native custody.
  Its correction preserves that existing contract and narrows only the already
  authenticated type-interface consumer. The separate generated-history failure
  `NativeGroupUnavailable(1374)` also occurs at the pre-change source; its cause
  remains under investigation. Neither failure may be erased by weakening the
  property oracle.

The initial review ran no new builds or tests. The incoming handoff reports
runtime/toolchain compilation, Q9 1/1, Q10 4/4 and residency 0/1 at `de12466597`;
Haskell selectors and the full `session` target were not rerun there. Retained
evidence is under `/srv/swarm/evidence/inanna/shared-warm-compiler-20261019/`,
especially `completion/` and `post-de124-review/`. Findings in other revisions
or dirty instrumentation checkouts retain their own scope. Subsequent receiving
evidence is in `root-native-scaffold/`, `verification-scaffold/`, `haskell-h/`,
and the area owners' handoffs. Component passes are not joined World execution
or release qualification.

Root checkpoint `65bd467d9d` compiled `tidepool-extract-cmd`,
`tidepool-toolchain`, `tidepool-runtime`, `exomonad-actor` and `exomonad-host`
(build `cfa42aa5-e949-4d21-a2dd-c23030d4e752`, launcher exit 0). The exact
World wire case and native-refusal preservation case each executed and passed
1/1; logs are in `root-native-scaffold/joined-65bd467d9d-*`.

Consolidated checkpoint `5d5c15b737` compiled all four affected extract-cmd,
toolchain, runtime and actor production/test consumers (build
`d638dba3-27d4-401e-97b8-2f7726513d99`, launcher exit 0;
`completion/engine/paired-observation-joined-check.log`). Its subsequent real
actor run with the target-HPT repair executed two cases: the source-domain case
passed; the authored-cell case reached a `WorldCodeGeneration` refusal on a
dependency. Haskell owns the phase/policy repair. No successful authored actor
cell, early Persona acceptance or whole-bundle performance result is claimed.

### Current receiving frontier

| Owner | Next concrete result | Acceptance boundary |
| --- | --- | --- |
| Haskell + engine | Resolve target demand with its actual dependency HPT; distinguish compiler-induced code generation from authored options | Actual first and second actor cells, unchanged dangerous-option refusals |
| Runtime + lifecycle | Weak installed associations and strong admitted/completed custody; original A/target B consumers | Lost successful birth delivery, abandoned B, held output, publication and transitive prerequisite retirement |
| Runtime + provenance owners | Canonical source authority independent of lexical/native resource roots | Self and reciprocal transfers release resources, foreign source remains exact, explicit cutoff still refuses |
| Haskell request + paired Rust receiver | Ordered full-language stages and genuine non-executable item proofs | Shipped mixed-cell template, precise prefixes, later error before any effect |
| Native owners | Correct TYPE-only projection and carry actual source provenance through native installation | Historical custody property, actual World seal, source-dependent and package-alias provenance |
| Queries | One typed actor/effect/request observation pin across all five verbs | Semantic query assertions and actual context mismatch refusals |
| Lifecycle recovery | Native owner-loss retention, exact force attempt and durable B reference | Actual retained attempt after kill/panic; durable import and no replay after process loss |

Receiver-only or source-only progress must remain labelled that way. Resume a
completed worker explicitly when assigning its next wave; a message to an idle
completed worker alone is not an implementation dispatch.

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
