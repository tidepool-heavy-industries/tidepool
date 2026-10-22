# N: one settled private frontier

Applies to code `29231ed323`. See [REVIEW-29231ed3](REVIEW-29231ed3.md).
Runtime admission/resident/paired publication is one owner's area. Actor adapters
may be delegated once this boundary is fixed. No second mutable World or event log.

## Facts that must stay distinct

- Immutable birth: execution/session/owner epoch, source, public baseline, leases.
- Session current World and compile association: the single authoritative state.
- Local program cursor: exact immediate predecessor, program identity and item index.
- Settled private frontier: owner-issued proof anchored at birth, bound to the
  exact current association and applied host revision after successful transitions.
- Accepted host results versus applied host results; retained histories versus
  currently visible winners; pending/unconfirmed cleanup versus absence.

The current Completed flag is not a cumulative proof. Local position.original_base
must retain its immediate-program meaning. Freeze and promotion must stop inferring
whole-history completion from that field or from final binding equality.

## State shape and transitions

Use one opaque frontier value in the existing private lifecycle owner. Do not
retain an ever-growing chain if replacing the current certified frontier suffices.
Its constructor is private to actual owner transitions and cannot accept arbitrary
current World + birth pairs. A handle to the real association is not another mutable
World. Maintain these logical states in enums rather than independent booleans:

| State/operation | Rule |
|---|---|
| Open, ready frontier | May reconcile accepted deltas and issue inert preparation offers |
| Prepared offer | Captures exact frontier/source/equal accepted-applied revision; owns no active slot |
| First semantic entry | At exact first-item admission, revalidate offer and claim one affine active program before native/declaration mutation |
| Running/resuming | Retain last settled frontier and exact active predecessor; later host acceptance does not invalidate that predecessor |
| Successful terminal | Prevalidate terminal evidence, then commit World/context/bindings, settled frontier and lease release together |
| Cancel before entry | Drop unused offer without changing ready state |
| Failure/uncertainty after entry | Preserve nonpublishable disposition; a later success cannot erase partial failure |
| Freeze | Refuse Running/pending/uncertain state before drain or mutation; enter transient Freezing exclusion, validate candidate, then seal |
| Frozen | Same intent is idempotent; no new starts, continuation or acceptance |

Use the existing prefix/lease owner for affine settlement. Do not add a generic
task scheduler. Dropping an active lease is Failed/Unconfirmed unless its owner
affirmatively proves no entry, effect or mutation and the same current association.
Only that clean non-entry proof permits returning the prior frontier; absence of
a result does not. No fallible lifecycle check may follow terminal adoption. Default after partial
execution failure: terminal private scope, fresh scope for new work. If existing
diagnostic continuation requires later cells, retain Failed publication disposition
through those cells rather than resetting it.

Native item, declaration-only and actual zero-item programs have real terminals.
An import is not necessarily zero items. Cancellation/drop must settle the active
claim; wire fail_authored_program or replace it, rather than leaving it unused.
Check lifecycle phase before any same-program continuation shortcut.

## Reconciliation and snapshot issuance

Synchronous acceptance remains after the existing cancellation fence. While an
authored program is parked, queue its exact accepted host delta; do not alter its
predecessor World. Fresh and prototype-reused internal HostCarrier preparation need
an explicit private-purpose path: quiescent current state or active program's
pinned predecessor, never a lifecycle bypass via scope alone.

At idle readiness, inspect accepted/applied revisions coherently. If equal, return
the existing frontier without scanning completed_values or copying a history.
Otherwise classify each unapplied delta against the actual current runtime winner.
An accepted result shadowed before first reconciliation is processed with a typed
superseded disposition, without reinstalling its old name. Prepare the remaining
exact deltas, then commit association, applied revision and frontier together under
the existing checkout/lifecycle synchronization.
Acceptance can occur without checkout: a final equality/phase check is necessary
on both empty and nonempty paths. If acceptance raced, retry pure reconciliation
or refuse the unused offer; never issue a ready r+1/r snapshot.

Do not hold the lifecycle mutex over compiler calls. No arbitrary whole-key guard
may reject legitimate host bookkeeping, and World pointer equality alone cannot
authorize unrelated stale binding/source changes.

Pending hidden operands are explicit inputs to readiness. Preparation accepts the
optional exact leased hidden receipt and issues snapshot and selected operand from
the same reconciled World. Do not issue a snapshot first and add an exemption later.
A later program must reselect that receipt or retire it. Unrelated unresolved mounts
remain blockers. Independent retirement may proceed while a specific program retains
its selected operand. Freeze admits no pending exemption.

Freezing is a transient exclusion in the existing lifecycle owner. While its
candidate is prepared outside the mutex, acceptance, new starts, pending registration
and private observation installation cannot mutate the selected state. Revalidate
exact association/source/revision before the single intent commit. Failure restores
truthful Ready state, or an explicit uncertainty state when restoration is not proven.

## Host projection, observation and shadowing

At idle, exact accepted-host commits can advance the settled frontier. Confirmed
retirement refreshes its association evidence without inventing authored completion.
While Running, mount/retirement bookkeeping may refresh the active predecessor
association without advancing settled progress past the unfinished program. Keep
that refresh distinct from a terminal advance; retain source and semantic selections.

Historical acceptance proves that an action occurred; publication selects current
visible winners. A later authored shadow must not require an older host name still
to be visible. Test shadowing both before first reconciliation and after an accepted
row was applied. Neither path may resurrect the old binding. Keep historical evidence
separate from current selection.

Observation/carrier support changes may preserve settled progress only through an
owner-approved transition that retains the exact semantic selections. If proving
that is awkward, use a temporary derived inspection World without replacing the
authoritative private association. Never blindly copy a local cursor across a
different World or clear progress as an incidental support rebuild.

Independent mounts retire in either order by exact identity. A stale same-name
receipt cannot remove a replacement. An unaccepted, unapplied mount has no World
row to remove. Required cleanup failure keeps its exact owner and fences publication.

## Activation and affine cleanup

Production activation already uses current snapshots; do not implement the obsolete
two finite-gate replacement task. Its actual remaining path:

1. Validate before pending registration where possible; register exactly once.
2. Before physical mount, an RAII reservation may undo only its own registration.
3. At mount commit, transfer custody to the exact mounted receipt. After that,
   clear pending only on acceptance or confirmed exact retirement. Failed cleanup
   retains an owner, not just a count and log.
4. Preserve input's original source A/root/renderer and recipient World B; no
   recompilation in B to fake A's identity.
5. Publish through existing Final/World selection intent with typed activation
   policy. Remove HostBindings/ActivationInput bypass for this consumer.
6. Keep request registry fencing, sessionInput replacement policy, cancellation,
   response identity and source publication races intact.

Actor accept/retire APIs must preserve the affine owner on refusal or borrow it
until commit. The saved N patch's `self.mounted` borrow/move is invalid; do not
cherry-pick it as finished. An async Drop logger cannot be the sole unresolved
cleanup owner.

## Required evidence and deletion

Use production-admitted captured source and next consumers:
two native programs without mounts; declaration then native; accepted Text then
native; native then host; real parked command then Job reuse; completed program
then discovery; actual zero-item program; two independent abandonment orders in
separate tests; freeze while parked then successful resume; deterministic
acceptance/snapshot race; failed later item with proven first-item progress;
host shadowing; A-to-B activation and post-mount cleanup refusal.

Publication assertions continue through a fresh public/private consumer.
Pending count alone is not a cleanup proof. Finalization refusal must leave the
still-valid original continuation usable.

Remove always-refusing finite private view/admission wrappers and unused private
CellSnapshotAdmission branch after porting real test consumers. Retain deliberate
ProtectedSetup controls. Remove synthetic positive authority factories after
migration; retain missing-source negatives. Compile runtime/actor unit and actor
integration/host consumers early. Do not call this lane done on component enums alone.
