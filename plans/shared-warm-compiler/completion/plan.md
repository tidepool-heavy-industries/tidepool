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

The published root checkpoint is `25316371c1` on
`integration/compiler-query-warm-final`. It includes root repair `e27921de24`,
engine cold-host repair `7ae7d2741d`, and the preview caller repair
`b2f02e4cc1`. All four affected production/test consumer targets compile.
Root fixture repair `115c81ee9e` has independent source acceptance and awaits
the next joined replay. These are intermediate integrations, not release candidates.
Compiler service, declaration publication, native issuer, lifecycle and query
owners are receiving reviewed changes on their area branches.
The baseline already has persistent GHC infrastructure; this campaign replaces
repeated reconstruction and competing cell paths, not a process-per-cell design.

| Evidence | Exact boundary | Remaining gate |
| --- | --- | --- |
| Root `d5d5686fda`: toolchain, runtime, actor and host production/test consumers compile | Selected native acquisition, source scope and ordered native assembly are joined | Actual full histories and current area folds |
| Engine `7ae7d2741d`: four consumers compile, build `051befb8-b7de-4f17-a2c2-ee4fb8dd77d0` | World host prototype retains its actual owner instead of requiring finite-only access | Actual private-input cohort executed 2: lifetime/refusal passes; positive fails later at `MissingFinalizedSource Scientific` |
| Actual query cohort at `b2f02e4cc1`: 2 executed, 1 pass, 1 failure | Source/base invalidation passes; the hot query and settlement now succeed before a later fixture admission failure | Root `115c81ee9e` uses actual source/private/public owners for that later history; rerun pending |
| Frozen declaration closure `053fd2cd41`, ordering repair `450483bd95`, final support `857afe6454` | Prospective readers validate origin/baseline pairing; the final binding API consumes issued custody instead of rediscovering dependencies. Codegen production/test compilation and 12 focused controls pass | Joined runtime materializer `3b6682bab7`, earlier reservation guard removal, and actual declaration-history execution |
| Retirement area `853ad26614`: production/test compilation and 2 actual rooted controls pass | Guard transfer, real owner loss and disposal uncertainty remain with their actual attempt | Affine worker return, final physical cutoff/stop and full force/recovery |
| Haskell optional display `79dcf9eab1`: 2 actual GHC controls pass | Optional rich display respects the authored extension capability | Finalized product role repair, mixed-stage and declaration histories |
| Scoped query `e1a1af0c0b`, producer `b3909545ec` | Runtime/actor and Haskell producer consumers compile separately; five projection/refusal and one context-mutation controls pass | Joined protocol 27/PREP5 execution, canonical resolver correlation, all-five semantics and malformed-output controls on genuine products |
| Scoped native target `93d82210be` | The target owns its exact demanded images; all five affected consumers compile. Seventeen of eighteen controls passed, and the corrected source-transaction fixture passes separately | Joined replay and actual current/historical causal source acquisition |
| Publisher body split `efe7a80a41`, rooted control `8a0e1ac303` | Actual unpublished A/B body returns the same guard; one real rooted control passes | Worker transport, terminal recovery, final cutoff/stop and full force/recovery |

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

Receiving work not yet fully qualified in the joined checkpoint:

- Declaration publication roots use actual exported values, instance dictionaries,
  class defaults and pattern implementations. Runtime adoption must retain their
  exact captured inputs, including hidden private operands, through the existing
  declaration dependency owner.
- Product finalization must distinguish fresh source, selected canonical SOURCE,
  retained prior Originals and TYPE-only evidence. The failing Scientific operation
  uses the actual successor's selected module/interface, not the previously inspected
  empty base World. Its selected proof is now carried into completion; finalization
  partition and actual execution remain pending. Unchanged interface bytes at another stage path do not issue
  a new implementation, and equal interfaces do not prove equal original lineage.
- Native installation advances its checked world's runtime association coherently.
  Do not refresh source observations, replace the world under its prefix or weaken
  unrelated stale-key checks.
- A declaration delta may reveal an ancestor binding after a local shadow is
  removed. Carry that actual winner and its retained checked row; the preceding
  visible-name map cannot reconstruct a previously hidden winner.
- Native installation must issue source contributions per installed program from
  its actual imports. Distinct members of a compilation batch must not inherit
  one another's unrelated source obligations.
- Cold recovery must preserve the requested proof-scope snapshot and namespace
  exactly, including an explicitly empty namespace. Narrowing materializations
  must neither discard proof-only scopes nor widen to parent custody.
- Scoped queries must share the held compiler source/checking owner, including
  the same resolved native annotations before demand renaming and final checking.
- Force retirement must block new affected admission while allowing already
  issued transfers to settle. The final physical recapture and first stop belong
  to one owner transition. Lost transfer ownership remains uncertain in the
  surviving terminal owner, even after the RunControl state is destroyed.

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
