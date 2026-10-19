# Contracts shared by every branch

Status: accepted implementation contracts; campaign amendments below are owner-reviewed decisions.
These semantic decisions take precedence over a leaf's convenient local shape.
Change them through the campaign supervisor and notify every affected owner.

## C1. Five facts that must remain separate

| Fact | Owner-issued meaning | Does not establish |
| --- | --- | --- |
| Custody | Exact immutable bytes and issued graph nodes remain available through real lifetime owners | Selection, native execution permission, or current source authority |
| Interface compatibility | These canonical interfaces agree for the relevant type/import use | Which implementation body is selected |
| Compiler selection | The request's explicit lexical, source and implementation choices | Permission to substitute another equal-interface implementation |
| Issued dependency edge | This body/group was issued with this exact required child/carrier | A choice of whichever child is newest or currently easiest to find |
| Executable demand and permission | Required groups for this operation, under its own issued permission and current runtime admission | All retained groups becoming roots, or a cache hit granting execution |

Preserve exact artifact/group binding identity independently of payload identity.
Several contexts may share bytes while having different dependency bindings. A
selected compiler namespace and a closed native dependency graph are different
objects; do not union all historical alternatives into the former.

**Required counterexample:** A and B implement the same module with equal
interfaces. B is explicitly selected, A is merely retained. A must not become
an optional root of the next compilation. If a surviving issued root requires A
as an exact child, that edge remains A. If that required edge truly contradicts
the consuming selection, refuse the conflict; do not relink it to B. Test both
possible explicit winners and unrelated extra groups on the retained child.

The current repair is `RequestNativeAvailability::for_compiler_roles` in
`tidepool/toolchain/src/declaration_context.rs`. Preserve its semantics even if
the implementation disappears. Do not weaken `validate_consumed_native_namespace`.

## C2. The selected input root belongs to runtime publication

`SelectedInputs` is a conceptual role, not a demand for a new public struct.
Extend the existing paired declaration/value/source publication owner so it
issues one immutable complete compiler-input root. It includes or references:

- Selected declarations and checked values, lexical visibility and original
  source/type/display evidence.
- Canonical compiler roles, exact Original selection, issued dependency graphs
  and native availability, with inherited exact group requirements kept distinct.
- Known home-owner membership and negative-resolution dependencies needed to
  distinguish home and package resolution, including zero-group home owners.
- Immutable interface/template facts and actual byte owners, plus incremental
  visibility/native/commitment indexes where current consumers need them.

The admitted world's home census is an explicitly issued exact module-membership
fact attached to the selected-row owner. Every admitted selected compiler row
carries its own authenticated home witness: canonical finalization validates its
own unit; a ValueInterface carries its checked reserved-value certificate; a
LexicalJoin carries its accepted reserved join and verified input witness.
The existing selected `(ArtifactBinding, Arc<ArtifactEntry>)` retains this
witness category; no public Home capability or name-prefix inference is needed.
Maintain a persistent membership index from actual selected row additions and
removals. Include zero-group and interface-only selected owners, value and lexical
joins. Exclude dormant custody and modules merely mentioned by another row's
home-unit set. The existing home-unit set for type-name classification remains
a separate fact. Type requirements must close through admitted selected rows,
not invent unissued census members. Inherited exact historical group dependencies
retain their own issuing classification independently of the current census.

A base-bound transition retains the exact old and successor selected owner
references. Role equality, payload identity or ArtifactView equality alone cannot
stand in for a transition: effective Original/canonical roles, scoped artifact
and group bindings, native permission/decisions and the home census all matter.

Capture is cheap: borrow the complete root, not rebuild it from every retained
value. Every mutation route updates it with the actual state transition:
declaration publication, value-only publication, alias, shadow, removal, source
adoption, fork, checked mounting and touched recovery paths. Audit all routes.
Do not maintain a second actor-current registry after commit.

An independently forked world owns a root, not a worker lease. Concurrent roots
can select different versions of the same logical module. Equal `ActorId`,
module names, canonical interface bytes or raw payload `Arc` identity cannot
issue or stand in for selection authority. Borrowing an already authenticated
private owner reference is valid and need not trigger another semantic rehash.

Fresh target/query executable demand belongs to the operation. Two operations
can share the input root while one inspects types and the other requests native
groups. Do not key a permanent actor world to one operation's demand or let a
prior Full/native request promote the next inspection's permission. Inherited
issued dependency requirements describe immutable code; they do not select every
available body for each new operation.

## C3. Deltas are certified changes, not caller patches

`CertifiedInputDelta` denotes an immutable, base-bound result issued by the
owners that validated the relevant new facts. Private constructors prevent
callers from combining a base ID with arbitrary additions. Typed transitions
distinguish raw inputs, admitted facts and publication candidates.

- Maintain persistent indexes for actual changed keys and reverse dependents.
  Include facts about absence: adding a home owner can invalidate a prior
  package-resolution answer even if it has no native groups.
- Immutable authentication, contextual compatibility and current runtime
  authority are separate checks with different invalidation scopes.
- Fully validate newly acquired untrusted evidence, including malformed unused
  rows. A demand-only traversal is not a substitute for input authentication.
- Publication chooses the successor root and bindings together. Failed or stale
  publication cannot publish a partially updated root. Recertification of a
  completed intent must not re-execute external effects.
- A certified private candidate may be realized and compiled under explicit
  preparation/receiving authority before publication. This is required for
  candidate reload validation and ordered intermediate cell prefixes. Only
  publication advances the actor's public selected root; worker readiness does
  not make a private candidate current.
- Use persistent maps/DAGs, not a linear chain that requires replaying every
  historical delta to read the current state.

Target hot cost is changed facts plus affected dependencies plus new source and
output work. Cold bootstrap/recovery, explicit full observations and globally
invalidating changes can legitimately inspect the full scope. Instrument those
cases rather than disguising them as ordinary incremental hits.

## C4. Physical inputs retain their issuing bytes

Borrow existing immutable interface rows/arenas. Produce only genuinely new
rows in fresh output staging. A path, digest or `Arc` count alone does not prove
the consumed bytes or their lifetime.

- Host and GHC must consume the same admitted input version.
- Keep input directory/arena owners through the actual compiler END and any
  uncertain cleanup evidence. Output publication must retain its own needed
  custody without pinning every input scratch directory by accident.
- Report selected bytes, unique bytes and pinned backing allocation size
  separately. A tiny slice can retain a large arena, lazy graph or TempDir.
- A pinned package universe must own the consumed closure or captured bytes;
  a cached path hash is not a freshness lease. Mutable/uncaptured dependencies
  keep their existing observation contract until an owner replaces it explicitly.
- Do not serialize raw GHC objects or rely on optional cache contents for recovery.

## C5. Worker realizations are optional and exact

The worker realization is an internal reusable product under the actual worker,
NameCache, package universe and closed selected world. Executable GHC/iserv
state additionally has an interpreter epoch. Share only immutable compiler facts;
make the attempt's mutable state private. No public `TcGblEnv` snapshot API.

Extend the existing owners, with this concrete protocol direction:

1. Bind the current endpoint and obtain an ordinary admitted/pinned compiler
   transaction. No actor retains it while idle.
2. Acquire the exact input realization with a **source-free** exchange in the
   existing versioned protocol. A valid worker-local reference can hit; a miss
   requests the host-owned portable root or a certified base-bound delta.
3. Hydrate/authenticate the missing facts and hold the realization internally
   through its actual use and END. A hot reference must not serialize the whole
   baseline just to discover it is already present.
4. Submit new user source under a fresh guarded operation using that realization.
   Runtime reservations, receiving allowance, request identity and cancellation
   remain per operation.
5. Finish and certify through the existing matching transaction END; current
   installation/execution/publication authority remains outside the cache.

Transaction BEGIN may already have been accepted during step 2. The safe miss
claim is that **no user-source operation has been accepted or run**, not that
no transport activity occurred. A source-free miss followed by hydration is not
a replay of a compile. Accepted user-source uncertainty never retries as a miss.

`tidepool-extract-cmd` owns framing, routing and lifecycle, not a new semantic
cache or selection policy. The toolchain issues portable inputs; the Haskell
worker owns decoded realizations. Any transport-owned table is limited to the
existing owner/lifetime bookkeeping required to carry those products.

Pool scheduling can prefer an exact/compatible warm world, then use any admitted
cold slot. Compatibility of partial worlds is not transitive: do not invent
permanent equivalence-class partitions. Retain current workload priority,
bounded queues, memory admission, worker rotation and cleanup.

## C6. Reload has a generation retirement boundary

This is a deliberate product change for changed explicit source reload. A
successful changed reload already installs a fresh notebook. The new contract
also ends permission to start new work from retired issued generations.

1. Prepare and validate the candidate without retiring the active world.
2. Default mode refuses if generation-dependent non-LLM actors/resources block
   retirement, reporting exact identities and observed states.
3. An explicit force option authorizes shutdown of that actual affected,
   generation-owned set at the fence. It is separate from accepting a changed
   tool surface/cache invalidation. No second confirmation-token workflow.
4. Atomically fence new old-generation compiler/install/resource admissions and
   dependent registrations; recheck expected generation and actual blockers.
5. Drain/cancel through existing owners, require confirmed cleanup for blockers,
   then publish source/tools/prompts/new notebook/root coherently.

Independent LLM actor worlds remain unchanged. Force does not kill all children,
close the enclosing LLM actor lifecycle, reset shared workers, or erase receipts.
Ordinary binder shadowing is not reload.

**Self-reload:** preserve the existing `Continuing` mechanism. The already
compiled initiating continuation can finish under retained physical custody.
Pure computation and operations justified by independent, source-neutral
authority may continue; new compiler work, installations or registrations tied
to the retired generation refuse. Enforce that at the real owners, including
source callbacks and transferred handles, not by tool name or optional
generation metadata. No new old-generation names may enter the replacement
notebook. A simple receipt can complete without a fresh old-generation compile;
a requested display that needs new specialization can refuse explicitly.

This transitional custody is not permission for indefinite new old-world work.
Existing operation cancellation/liveness applies; do not wait for the initiating
reload operation itself to finish before returning its result. Tests must prove
both no deadlock and no fresh authority after the fence.

Candidate validation failure stops nobody. Partial retirement is not rollback:
if X stopped and Y remains uncertain, report that state even if publication did
not happen. Do not automatically restart X or replay effects. The existing
storage/cleanup owner must retain a truthful recoverable state if final
publication is uncertain.

An unchanged reload preserves its existing no-change semantics unless an
independently reviewed incompatibility requires otherwise. No stealth reset.

## C7. Reuse normal GHC semantics, not stale/editor answers

Persistent HPTs and warm processes alone do not remove normal import processing.
Any checked-import reuse experiment must account for lexical scope, aliases,
instances, defaults, orphan/family consistency, original identities, current
target/boot semantics and compile-time execution. Fresh mutable knots, hooks,
TH state and finalizers cannot be borrowed as immutable cached answers.

Only pure, fully evidenced intermediate products are reusable. An editor's stale
answer is never execution authority. TH/CPP/plugins/untracked IO keep the current
non-replay constraints. If an experiment cannot maintain ordinary module
semantics, stop promotion and document the measured remaining frontend floor.

## C8. Integration proves the change

Every significant invariant needs a production constructor, a consumer, a
positive/refusal control and a planned obsolete-API deletion. Differential
reference implementations must not share the policy they are meant to verify.

An independent reviewer checks exact commits. Then a joined production history
checks the merged commit. Compile-only or zero-selected runs are not passing
behavior. Durable format/commitment changes get explicit versions and truthful
recovery/refusal, not compatibility adapters that silently guess old decisions.

Do not add a parallel framework, selector, registry, test authority factory or
full-scan escape hatch to make a leaf pass. A conservative cold path can remain
where the contract genuinely needs full validation; warm fallback frequency must
be measured and reported.

## Campaign decision receipts

- 2026-10-19, C2 exact home census: host-input supervisor and independent census
  reviewer traced canonical/value/lexical production issuers. Campaign directly
  inspected selected-row ownership and the broader OriginalValidationInventory
  census before accepting the rule above. Consumers: host selected root/deltas,
  package-negative resolution, worker exact-world inputs and qualification oracle.
  Implementation and executed oracle acceptance remain pending; this decision
  does not certify the old available-original census as equivalent.
- 2026-10-19, C6 provenance direction: generation dependencies travel explicitly
  in the owning actor/session context through callback transfer; migrate actual
  constructor callers rather than encode authority in source-root names or add
  an actor-current registry. Lower runtime gates must retain crate dependency
  direction. Absence never grants source-dependent admission. Exact type/schema
  and production fence/stop result remain under implementation review.
