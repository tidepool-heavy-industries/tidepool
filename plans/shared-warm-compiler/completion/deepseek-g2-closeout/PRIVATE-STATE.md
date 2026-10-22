# N: one private-state progression

Primary outcome: a private execution can use its accepted results in subsequent
cells, retire temporary inputs and publish without mixing birth and current
compiler state. Secondary outcomes: truthful progress, cancellation/cleanup
custody and removal of obsolete finite-view APIs. This replaces N's earlier
boolean/progress patch instructions; [CONTRACTS](CONTRACTS.md) still applies.

## Why this shape

`PrivateExecutionAdmission::compiler_basis` captures birth state. Planning later
cells still borrows that World, while the runtime key and binding table advance.
Accepted host rows enter the compiler only at freeze. Retirement records then
try to replay old association chains against the newer current World. These are
different manifestations of missing current-state ownership.

There is an important constraint: command acceptance can occur while the enclosing
checked computation is parked. Its already-issued successor expects its original
predecessor. Immediately adding host rows to that World introduces another merge.
Keep synchronous post-cancellation acceptance, retain its exact pending delta,
settle the checked computation, then apply accepted host rows before new authored
preparation or freeze. This is a bounded lazy projection, not a second registry
or a background best-effort update.

## 1. Separate birth authority from current readiness

Keep immutable session/execution identity, owner/epoch, source admission,
public baseline and private scope lease. Birth compiler facts are used for the
final three-way publication comparison, not as a general current-World accessor.

The existing session checkout owns the current private World association,
runtime key and binding/source selection. Issue an opaque private preparation
snapshot there, after accepted-delta reconciliation. It binds:

- the execution and live source/target authority;
- the current World and runtime view/key witnesses;
- the accepted-host revision that has been applied;
- the preparation purpose and its actual predecessor.

Carry that same snapshot through compiler plan, runtime admission, checked
operands and start. If preparation uses a pending hidden input, the snapshot
issuer takes that exact receipt and returns the snapshot and operand together,
after reconciliation. Do not issue the operand against a different earlier World.
A later accepted delta invalidates an unused snapshot even
when the runtime key did not change. Re-admission/start checks its captured
accepted revision under the lifecycle guard. Already-started work retains its
pinned predecessor; later host acceptance does not retroactively invalidate it.

Replace the birth `compiler_world`/`world_basis` route for new private cells.
Do not add a second shadow current World to the admission object. Existing public
World/inspection snapshots retain their own meaning; an observation alone must
not mint readiness for authored work.

## 2. Typed host acceptance and application

Retain hidden versus visible purpose in one enum, rather than two independently
optional mount fields. A hidden input is available only through its exact checked
operand authority. A visible result becomes accepted only through its original
post-cancellation owned-step boundary.

The existing private lifecycle guard must linearize accept, readiness capture/start
and freeze. Keep accepted intent/progress in that owner and the actual current
World in the session; do not add another mutable current-World copy. Couple phase
and accepted/applied revision transitions rather than coordinating independent
`frozen`, journal and pending-set locks by caller convention.
Acceptance moves a pending visible receipt into an accepted-unapplied state and
advances its revision. It retains the exact binding, authenticated type proof and
cleanup custody. It does not claim the compiler has already applied the row.

An acceptance refusal must leave the original retirement receipt owned and usable.
`keep_binding` must not consume the write and then drop into generic cleanup on
error. Use a borrow-and-commit transition or a refusal returning the original
pending owner; disable cleanup only after success. Do not clone an affine receipt
to sidestep this requirement.

Before a **new authored program**, or freeze, the session owner:

1. Requires no active authored program. New authored preparation may retain its
   explicitly selected pending hidden input through the exact operand authority;
   that input is not a readiness failure. Other unresolved mount/retirement
   outcomes remain distinguishable. Freeze has no hidden-input exemption.
2. Validates accepted deltas against their exact current binding/type evidence.
3. Constructs the successor from those deltas using the existing World operation.
4. Commits World association, current runtime witnesses, applied revision and
   host-progress evidence coherently, then issues the ready snapshot.

Apply each accepted row once. Do not rebuild an overlay of every historical
completed value on every query or freeze. Build the candidate before mutating
state; source/cancellation/fallible rejection must preserve a truthful owner.
Do not hold an admission mutex across compiler calls. If candidate construction
releases the lifecycle guard, commit checks the captured revision again. Retrying
pure preparation is distinct from replaying effects and must never replay them.

Internal HostCarrier preparation is a deliberate exception to authored readiness.
Its owner has two explicit cases: a quiescent private state before any authored
program, or the pinned current predecessor of an active program while its item
is parked. It performs its limited carrier recipe without advancing authored
progress or draining that program's accepted host deltas. Both first preparation
and prototype reuse must retain the execution/source association; neither a
generic busy check nor bare scope-only admission is sufficient. The former
deadlocks command handling; the latter bypasses the new owner boundary.

## 3. Track checked progress at the real transition

Use exact admitted program/prefix/item identity, not a freely mutable
Started/Completed flag. Multiple prepared offers may exist; only one authored
program may own advancement of a given private state at a time. Claim it atomically
on the first item (or explicit empty-program commit), including declaration-only first items, rather than when an
offer is merely prepared. Keep its ownership across all items, effect suspension
and continuations. Release it only after the final successful item or an explicit
failure/cancellation settlement; unresolved cleanup remains unresolved.

Give the actual empty/import-only program an explicit owner terminal transition
too. Today `begin_world_cell_program` records Started and returns no prefix for
an empty item list; only native settlement records Completed. Test empty input
and explicit/empty imports through their real compiler output, rather than making
a dummy native item or inferring successful no-work from absence.

A second stale or overlapping program start refuses. Internal HostCarrier work
is not another authored program. Do not drain accepted host deltas or require the
original accepted revision to remain unchanged at every internal item boundary.
This would invalidate a multi-item program after its own command accepted a Job.

Native settlement and declaration-only adoption both commit progress only after
all fallible validation/adoption succeeds. Failed later settlement cannot leave
the earlier item represented as successful completion of the new item. Started
or uncertain work is not Unchanged simply because no successor was installed.
Require the current predecessor association to exist; do not accidentally accept
absence through `Option::is_some_and` while checking only mismatch.

The started item's exact World/input custody remains pinned across legitimate
host mount bookkeeping. `CheckedTurnCompletion::settle` already preserves this:
it uses the retained predecessor, and successor adoption reads the current runtime
key. Keep that behavior. Audit whole-key guards at next-item admission and
declaration adoption as well as native install: permit
only the owner-issued mount changes that the production checked path already
supports, never arbitrary concurrent source/binding edits. A new accepted-delta
revision fences a new start, not completion of its already-running predecessor.

## 4. Retire ownership, not an old snapshot

Retirement uses exact execution/mount/binding identity, scope/source authority,
accepted-versus-unaccepted state and the current session. The original mount's
`after` association need not remain the newest association: another mount or a
successful cell can legitimately follow it.

Commit retirement and current runtime/World bookkeeping together. An unaccepted
visible row that was never applied has no compiler row to subtract. Accepted
bindings cannot be retired by a late pending guard. Same-name replacement must
not be removed by an older receipt. Keep exact result/cleanup evidence.

Delete `PrivateHostRetirement::following_key` and the replay of retirement records
as publication authority once current owner transitions cover these callers.
Receipts may remain for diagnostics; a history of independently applied retirements
must not be required to have one contiguous key chain or one unchanged World.

## 5. Freeze and publish

Reconcile at quiescence, prepare and validate the final intent, then seal it under
the same lifecycle exclusion that prevents new acceptance/start. A pending-work
refusal must not leave `frozen=true` with no final intent. Successful freeze is
idempotent for the same intent; uncertainty has an explicit terminal outcome.

Keep original/private/latest-public selection composition, exact promotion
witness, cancellation claim, durable commit and post-effect recovery semantics.
The existing source-selection delta repair is useful; do not replace B with A's
snapshot again. Public-only custody is validated against public evidence.

One bounded N1 correction: if latest public state deleted an admitted source
domain, private deletion-only changes are already satisfied and cannot recreate
it. Private additions or changed retained entries into that removed domain
conflict. Preserve latest current-domain relabeling. Test both selected keys and
authored origins; do not merely loosen the failing assertion.

## 6. Move every real caller

Ordinary request activation currently calls the guaranteed-refusing
`compile_view_for_execution` at both input mount and activation publication.
Move both to the scoped World/current-owner route. Its `OriginalLiveInput` retains
caller source A, exact request/root/type and original display authority; recipient
private state belongs to B. Preserve both authorities. Do not require A == B,
recompile the original renderer against B, or treat retained bytes as a grant.

This is more than replacing two guards: `mount_activation_input` returns a
different mounted receipt and currently bypasses accepted/applied host progress.
Have the common private-state owner register the actual input's root and interface
through its typed activation case, then publish runtime and World together under
the existing request-registry fence. Migrate the old HostBindings/ActivationInput
ticket that lacks a World intent. Preserve its special sessionInput replacement,
request identity and reply lifecycle semantics. Do not fabricate checked execution
to publish an input. This is G2 ordinary behavior.

Remove `execution.view()` and finite private preparation forwarding APIs when
their real callers have moved. Port useful remaining tests through production
World admission. Delete the dead private branch of the test-only cell snapshot
helper. Replace `WorkbenchCompilationAuthority::for_test`'s owned-source bypass
with the real captured resolver on all joined positive histories. Keep the
missing-resolver refusal.

## Suggested delivery cuts

1. Current snapshot, accepted-delta reconciliation and exact checked progression
   together; Text-before-publication and parked-Job histories.
2. Retirement/freeze transaction, two-mount interleavings and refusal custody.
3. Activation migration, dead surface deletion, N1 selection edge and joined
   public/private histories. Compile each directly affected test consumer early.

N owns runtime admission/resident/paired-publication hunks. An actor leaf can
work against the agreed snapshot contract; a test leaf owns independent histories.
Do not split the same lifecycle state machine among multiple uncoordinated writers.
An enum is valuable when its production constructors enforce a transition, not
when it merely renames independently mutable fields.

Use existing structured tracing to report execution/program/item identity, phase,
accepted/applied revision and expected/observed World on refusal. Keep refusal
variants typed and preserve the original cause plus cleanup disposition. Do not
dump entire Worlds or make rendered log text control behavior. These fields should
make the next failed join diagnosable without another full trace-capture wave.
