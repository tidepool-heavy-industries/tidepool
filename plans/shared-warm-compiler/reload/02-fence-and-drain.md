# 02 — Fence, drain and generation publication

Baseline: `70a92c65a02a591f76590daf4e5bfa3e6fcbf723`. This child extends existing
reload, lifecycle and runtime publication owners. It depends on child 01's exact
dependency issuance and admission gates. The worker owns this sequence; an
independent reviewer owns race/failure histories and self-reload liveness. This is
future implementation work, not an implemented shutdown or publication claim.

## Current owner path

`CoherentReloadOperation::run` prepares, creates/borrows the existing
`PublicationDecision`, calls `ActorGenerationPublisher::publish`, then delivers
Published/Refused/Unconfirmed. Continuing publication uses the original execution
control. `ActorGenerationPublisher::publish` checks the exact expected current
generation and machine owner under its existing machine/current-state owner.
The runtime's `commit_notebook_generation_continuing` retains the execution's
decision until the store confirms installation. Extend this path; do not add a
second selected-generation journal or generic saga supervisor.

Sources: [reload composition](../../../exomonad/actor/src/resident_actor/owned_workbench/reload.rs),
[generation publication](../../../exomonad/actor/src/resident_workbench.rs),
[runtime commit](../../../tidepool/runtime/src/session/persistent.rs),
[publication decision](../../../tidepool/runtime/src/session/publication.rs).
Actor shutdown/resource settlement stays with `LifecycleHandle`,
`KernelContext::retire_resources`, the existing run directory and
`RetainedActorExit`/`ResidentCleanupOutcome`; source dependency narrows which
generation-owned entries they act on.

## Affine attempt and state transitions

Use private attempt state attached to existing owners. It retains prepared
candidate, exact expected A, force Boolean, coordinator identity and immutable
stop observations. The attempt's fence/claim is consumed or explicitly settled;
dropping a future must not reopen admission or forget started cleanup.

| Transition | Issuing owner action | Observable result |
|---|---|---|
| Open A → preparation | Capture complete source/candidate using existing preparation owner | Invalid candidate: A selected, no retirement actions. No-op: A bindings/services preserved, no fence. Surface refusal: no retirement actions. |
| Prepared B → fenced A | Under generation admission owner, recheck expected A and close new dependent compilation/install/admission **and registration/transfer**; capture actual affected generation-owned set | This set is internal authoritative input, not a preview approval token. Resource startup finishing concurrently either joins it or is refused and cleaned by its issuer. |
| Fenced A + `force=false` + blockers | Return typed blockers and consume the unmutated fence by reopening A under its generation owner before returning | A remains selected and operational; no service stopped by reload. Retained read-only observations remain available. Pre-fence pending registrations belong to the captured set and continue under reopened A. |
| Fenced A + force or no blockers → drain | Cancel/drain exact dependent non-LLM actors/resources through existing owners; settle accepted compiler work/END and prior operations | Per-owner confirmed/stopped/still-running/unconfirmed observations retained. Independent LLM worlds remain outside the set. |
| Confirmed required drain → publish B | Recheck expected selected A and existing publication claim; atomically commit B's source/install/prompts/fresh notebook context using current durable/ephemeral owner | B selected and usable only on actual commit/installation acknowledgement. A new dependent admission remains closed. |
| Publication settled → receipt/continuation | Existing `resume_source_answer` delivers its retained outcome, then already compiled A continuation may run under child 01's limits | No wait for the coordinator's own terminal execution. Physical A custody survives only actual outstanding admitted owners. |

`force: true` authorizes the affected generation-owned set **at the fence**,
including entries created after candidate preparation but before that atomic
capture. It does not require a previous default refusal or exact user-supplied
name list. The receipt reports X/Y/Z actually affected. Actors merely descended
from the LLM application but owning independent worlds are not part of the set.
Unknown source-dependent resources refuse safe retirement instead of being
assumed independent. A failure to inspect a resource is not an empty inventory.

The fence distinguishes pending pre-fence registrations from new attempts. Admit
and record the pending registration at its existing owner before native startup;
it then belongs to the captured affected set even if installation is incomplete.
Default refusal releases that unmutated fence and lets the pending owner continue.
New post-fence reservations refuse before allocating a source-dependent resource.
If observation itself is unavailable and the owner cannot prove an unmutated
release, return explicit admission/cleanup uncertainty rather than a normal
Blocked result promising A is operational.

One serialized generation gate must order capture with registration, but it need
not hold a machine checkout or actor-state lock while awaiting GHC, external
cleanup, child stop or store IO. Issue an affine fence/attempt fact under the
owner; release the borrow; compose existing awaits; reenter the same owner for
checked transitions. Preserve Ractor's existing serialization and lifecycle;
there is no parallel mutable actor owner or second scheduler.

## Register, activate, cancel and publish races

`KernelContext::spawn_resource` currently performs lifecycle resource admission,
prepares a linked actor and registers it in the existing directory. Extend its
real issuance/registration seam and scoped-worker/start/source-transfer equivalents
with child 01's dependency. A pre-fence reservation cannot silently publish after
the set was captured. It must either have been counted as pending owned work,
finish into the fenced set through its owner, or refuse and retain its actual
startup cleanup. A stopped PID, dropped future or registration error is not
confirmation that its native roots/callbacks/compiler inputs were released.

At activation, a retained callback/typed-service handle that can create new
generation-owned work rechecks the same dependency gate. A response to an already
admitted request can settle its exact original destination without installing a
new source-dependent service. Do not confuse new admission with transfer/use of
already issued immutable result evidence. Keep exact type/original evidence on
both paths.

Other A compiler operations that won admission before the fence remain owned
through original transaction END. Wait/cancel them using their real compiler
registration; no new token, direct fallback or synthetic Clean close can authorize
publication. Cancellation after native response but before END preserves inputs
until settlement. Independent compiler preparation waiters cannot cancel their
shared producer just because reload is retiring one consumer.

Existing A external effects are not rolled back. Freeze/reuse their final intent
and retain receipts, or settle cancellation/refusal through the execution owner.
Do not reexecute a cell, callback or stop operation to rebuild an acknowledgement.
Any publication from an already admitted A invocation remains confined to its
original private/public owner; it cannot republish A bindings or names into B.
An old policy handle issued but unused before the fence cannot admit a new call.

## Self-reload exception is physical, not new authority

The exact initiating invocation and receipt delivery must remain resumable without
appearing in the set whose *terminal invocation* reload waits on. Otherwise the
reload operation waits for its own return. Settle the reload operation's compiler
and cleanup work normally; exempt only its retained already compiled
control/continuation custody. Do not grant a general exemption for all requests
with the same actor label or generation.

After the barrier, that continuation may run pure old code, perform independently
authorized source-neutral operations, and inspect/settle existing receipts. Fresh
compiler tickets, installation, source-dependent callbacks/typed-service starts
or registrations refuse at actual owners. Handler-specific input checks decide
whether display/IO is neutral; no new global effect framework. Unclassified
source-dependent registration is refusal. Pure nontermination is possible:
publication B can be known while A's continuation still retains bytes until
existing cancellation/budget/terminal cleanup. Report this retained physical
state separately from A's closed admission.

## Failure obligations

| Failure point | Required report/state |
|---|---|
| Source/candidate/typecheck/surface failure | No fence-side stop effects; A remains operational with notebook bindings preserved. |
| Default blockers | Exact affected references/reasons in a typed refusal; no destructive stop. Force is a separate Boolean, not implied by surface cache consent. |
| Cancellation before drain | If no stop or publication side effect began, consume the unmutated fence by reopening the still-selected A. Otherwise settle actual actions first and use the confirmed/uncertain transitions below; never drop-and-forget. |
| X stopped, Y refused with known settled outcome | No B publication; exact A remains selected. After all attempted stop outcomes are known and publication is known not committed/uncertain, the owner reopens A and returns partial non-publication: X stopped, Y still live/refused. It restarts nobody. |
| X stopped, Y cleanup unconfirmed | No B publication is claimed; retain FencedNeedsSettlement with actual stopped/uncertain outcomes. Do not reopen source-dependent admission until reconciliation establishes all actual outcomes and no publication uncertainty. |
| Drain confirmed, clean publication refusal | A remains selected; consume the fence by reopening A only after the owner confirms exact selection and all stop outcomes. Report stopped resources as stopped; do not restart them. |
| Cleanup inspection/ownership loss or uncertain stop | Retain fenced/unavailable admission and per-owner evidence until reconciliation. Absence is not settled cleanup. |
| Store commit/installation acknowledgement uncertain | Existing publication-unconfirmed/recovery-required state blocks new dependent admission. Do not return Published or automatically replay reload; reconcile exact durable selection first. |
| Commit won, caller cancelled or continuation failed | Committed B and reload receipt remain authoritative. Retain original continuation failure/cancellation independently; no rollback or repeated reload. |
| Coordinator/host crash | Existing durable generation/operation owners determine selected state and cleanup; never infer “not published” from missing in-memory attempt. No second recovery log; persist only owner-required state through its existing transaction/journal. |

The owning terminal states are consequently OpenA(unmutated/default refusal),
OpenAWithStoppedResources(confirmed partial/non-publication), RetiredAWithSelectedB
(confirmed publication; coordinator may retain bytes), FencedNeedsSettlement
(cleanup/ownership uncertainty before a known commit), and PublicationUnconfirmed
(selected durable result must be reconciled). These descriptions are private
states of existing generation/lifecycle/publication owners, not a new public
scheduler. Reopening compares the exact still-selected A and attempt fence;
a superseding generation never gets reopened or overwritten by A's late cleanup.
PublicationUnconfirmed uses existing unavailable/recovery-required admission,
not an inferred A or B. Once reconciliation knows outcomes, transition through
the corresponding confirmed state and preserve the stopped-resource report.

Required historical dependencies remain owned throughout accepted A work, even
when retirement is desired or B is already selected. Follow the rewrite handoff:
do not remove a demanded old native child, expand all retained alternatives into
B's roots, or weaken consumed-namespace validation. Physical release occurs only
after actual owner settlement and last reference, independent of cache eviction.

## Implementation/reviewer waves and evidence

| Wave | Worker scope | Reviewer/acceptance obligation |
|---|---|---|
| R2a — Fence and actual-set capture | Existing generation owner plus lifecycle/resource directory registration; consume R1 checked dependency constructors | Deterministic barriers race startup, register, transfer and activation against fence. One operation is either counted or refused with retained cleanup; never missed. Same-label different incarnation and independent LLM/fork controls. |
| R2b — Default and force drain | Reload composition, existing per-resource stop/cancel/drain and confirmed cleanup aggregation; preserve self coordinator | Default stops nobody; force works without preview and affects actual fenced set. Late registration, partial X/Y cleanup, uncertainty and owner loss have typed outcomes. Reviewer checks no broader actor lifecycle close. |
| R2c — Publication/continuation/recovery | Existing `ActorGenerationPublisher`, runtime continuing/terminal commit and source-answer receipt path | Joined authored reload→receipt→pure continuation→neutral read→blocked new old work→fresh B call. Crash/cancel before/after commit; actual old/new selected root observed through production store. |

Compile the changed actor, runtime and direct host test consumers at each wave;
the schema/API wave adds its own dependencies. Use counted production tests and
an independent state-machine model for cheap interleavings. The model keys exact
owners, fence admission, registered resources, selected generation and known stop
effects; it must not call production fence logic as its oracle. No arbitrary
sleep is a proof of a race being exercised.

Delete any caller-side “list then stop” path that duplicates selection or can miss
a registration race. Delete assertions treating a stop request, PID disappearance,
registration failure, snapshot drop or cache eviction as confirmed cleanup. Keep
existing shutdown/retirement evidence types and owners; do not add a reload-only
teardown registry, journal or broad effect classifier.

Evidence per leaf: base/final revision and complete diff, exact worker/owner
identities, affected set at fence, confirmed/partial/unconfirmed cleanup receipts,
compiler action/END outcomes, selected durable generation before/after, actual
test counts/exit/logs and a joined continuation result. Fold leaves to this area,
review joined consumers, then hand the area candidate to campaign integration.
