# Reload retirement

This area plans changed-source reload as a prepared generation transition with
explicit retirement of generation-owned executable services. It is a design and
delivery plan, not implemented or qualified behavior. Source baseline:
`70a92c65a02a591f76590daf4e5bfa3e6fcbf723`.

Changed reload already resets the replacement notebook. The new contract closes
old-generation admission and, by default, refuses when dependent generation-owned
non-LLM actors/resources block retirement. `force: true` authorizes stopping the
actual affected generation-owned set captured at the fence. It requires no preview,
approval token, or prior blockers receipt. Independent LLM actors and their captured
worlds are unaffected unless a separate explicit operation propagates adoption.
This policy applies to explicit changed reload, not ordinary binder shadow.

Preparation runs first. Invalid source stops nobody. Unchanged reload preserves
bindings, opens no retirement fence and does not stop services. The existing
`accept_cache_invalidation` surface policy remains independent of `force`.
Default blocked retirement consumes its unmutated fence and reopens A before
returning. Confirmed partial forced cleanup can reopen the still-selected A only
once every stop outcome and non-publication are known; stopped services stay
stopped. Cleanup or durable-publication uncertainty keeps dependent admission
explicitly blocked until owner reconciliation.

The implementation sequence is:

1. [Retirement contract](01-retirement-contract.md): issue exact generation
   dependencies, distinguish new admission from retained physical custody, and
   close source-bearing callback bypasses.
2. [Fence, drain and publication](02-fence-and-drain.md): capture the affected set
   under the existing owners, refuse or stop/drain it, then use existing atomic
   generation publication and failure/recovery semantics.
3. [API and acceptance](03-api-and-acceptance.md): expose the independent Boolean
   and typed outcomes, migrate obsolete late-admission behavior, and qualify the
   real authored/hosted workflow.

The area's supervisor proposes and coordinates cross-crate contract changes;
the campaign supervisor arbitrates the shared contract. The area owns direct
source review and integration. Each child describes implementation/reviewer waves for future
work. Leaves integrate into an area branch; the reviewed area result integrates
into the campaign branch. Workers retain base and final revisions and present
the complete base-to-tip delta. No leaf independently changes main or deploys.
Compile/test and release qualification are separate from authoring these plans.

## Current owners and preserved contracts

| Owner | Production entry points | Area obligation |
|---|---|---|
| Source preparation | `ReloadPreparation::prepare` in [reload_preparation.rs](../../../exomonad/actor/src/resident_workbench/reload_preparation.rs) | Complete candidate validation before retirement side effects; keep no-op and surface-policy decisions. |
| Reload composition | `CoherentReloadOperation::run`, `coherent_reload_operation` in [reload.rs](../../../exomonad/actor/src/resident_actor/owned_workbench/reload.rs) | Carry force and the exact expected generation; compose existing owners rather than introduce a reload scheduler. |
| Selected generation | `ActorGenerationPublisher::publish`, `ActorRequestSnapshot`, `InstalledToolsState`, `prepare_actor_generation` in [resident_workbench.rs](../../../exomonad/actor/src/resident_workbench.rs) | Existing current pointer and publication checks remain authoritative; replacement uses isolated scope. |
| Runtime commit | `commit_notebook_generation`, `commit_notebook_generation_continuing` in [persistent.rs](../../../tidepool/runtime/src/session/persistent.rs) | One durable publication owner; keep selected state, installation acknowledgement and uncertainty distinct. |
| Execution/cleanup | `WorkbenchExecutionControl`, `CompilerWorkTicket`, `LifecycleHandle`, `KernelContext`, existing run/resource directory | Fence admission at issuing consumers, drain through actual owners, retain confirmed versus unconfirmed outcomes. |
| Authored API | [effect schema](../../../bridge/protocol/src/effects/reload.rs), [Tidepool.Reload](../../../bridge/haskell/lib/Tidepool/Reload.hs) | Generated contracts and authored defaults/presentation agree; no hand-edited generated replicas. |

The baseline explicitly permits a previously issued A request to start a new
Haskell call after B publishes. The test
`coherent_reload_replaces_the_notebook_and_prompts_without_rewriting_issued_work`
in [coherent_reload_acceptance_tests.rs](../../../exomonad/host/src/actor_host/coherent_reload_acceptance_tests.rs)
exercises that positive path. This area's intentional change is a typed refusal
for fresh old-generation compiler/install/registration authority after the
barrier. It does not erase an already admitted computation or silently replay it.

## Self-reload and retained history

Self-reload remains Continuing. The owner settles the reload receipt before
resuming the already compiled old continuation; it cannot wait for itself to
terminate before publication. Pure old code, independently authorized
source-neutral operations, and settlement of existing receipts may continue.
Fresh old-generation compilation/installation and source-dependent registration
must refuse at their real owner. A display requiring new specialization can
refuse; a simple precompiled receipt display remains possible. An old callback
must carry its dependency through transfer, including `SourceCallback` snapshots
whose `generation()` accessor returns `None`.

This is transitional physical custody for admitted work, not permission to keep
two generations open for new work. Haskell need not terminate promptly. Keep the
old code, original contexts and exact dependency graph until actual terminal
settlement or existing cancellation/cleanup. Source-neutral classification is an
owner property, not an effect-name allowlist. Unclassified source-dependent
registration refuses.

The required rewrite handoff is
`/srv/swarm/evidence/inanna/persona-1895-launch-20261019/compiler-failure/HLS-REWRITE-HANDOFF.md`.
It establishes that retained custody, selected compiler roots, issued native
availability and required historical dependency edges differ. Retirement changes
future admission; it cannot delete a required child of code still admitted in an
independent or draining world. Preserve `RequestNativeAvailability::for_compiler_roles`
and strict consumed-namespace validation. Do not make a retained equal-interface
alternative an optional compilation root or weaken a genuine conflict check.

## Integration dependencies and acceptance

This area consumes the campaign's exact-world identity and current-context owner;
it does not create another actor-current registry. Host-input work must retain
admitted old roots until settlement. Warm-worker work must treat snapshot/cache
references as acceleration, never admission or cleanup evidence. Loader epoch
rotation and old image release remain with their existing owners.
An authentic prospective B root can compile under explicit preparation/private
purpose before publication; otherwise prepare-before-retire would be impossible.
Only the existing successful runtime publication selects B as the actor's public
root. A failed publication may leave immutable worker/cache facts unused without
selecting B or authorizing a fresh old-generation request.

Before integration, the supervisor inspects issuance through old-policy handle,
callback transfer, compiler ticket, install, source registration and final use.
Independent review covers one primary goal—no new old-generation authority after
the fence—and adjacent questions: self-reload progress, independent-world
isolation, and honest partial cleanup. A clean text search is not acceptance.

Required joined histories: invalid/no-op/changed reload; default refusal; force
without prior preview; registration racing the fence; partial cleanup; unconfirmed
cleanup/publication; stale issued handles; source callbacks/forms/typed services;
continuation failure after committed reload; self-reload pure and neutral effects;
retired compiler/install/registration refusal; independent LLM child/fork survival;
and recovery of exact selected B under edited draft C. Exercise historical native
dependencies while old admitted work lives and verify release only after its
last actual owner settles.

Retain exact source and bundle hashes, commands, selected/executed nonzero counts,
compiler and lifecycle settlement reports, old/new selected generation, affected
set at fence, per-owner stop outcomes and log/report locations. Separate compiled,
executed and unqualified obligations. These plan documents have no implementation
or performance qualification evidence.
