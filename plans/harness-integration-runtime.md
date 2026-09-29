# Concurrent resident notebook: runtime handoff

Proposed engineering design, 2026-09-28, traced against baseline `b2de366b2`.
**None of the concurrency or publication changes below are implemented.**
The accepted behavior is in
[`harness-integration.md`](harness-integration.md); this handoff identifies the
current owners and the smallest source changes that can preserve capture identity
and completion-order publication. It does not authorize replay of effects or
restoration of live heap values after host loss.

The implementation branch now includes per-execution hosted-call tracking
(`1059890d1`) and transitive binding retention (`41583b5eb`). The baseline
analysis below explains the remaining scheduling and publication work; the
single hosted visibility slot described below has already been replaced.

## Baseline execution and the blocking seams

### Foundation reconciliation — preparation at `4a8088c93`

The first-tree PRD now separates sequential embedding (M1) from concurrent
private executions and reusable captures (M2). This inventory is design input,
not authorization to enable concurrency before foundation acceptance and the
per-execution ownership review. The source descriptions below predate active
foundation candidates; reconcile them against the accepted integration revision.

| Seam | Preparation / required decision |
| --- | --- |
| `LocalActor` and hosted-call visibility | The in-flight foundation candidate moves one owned behavior into an active task and retains accepted controls independently of caller futures. This makes control responsive; it does not make the behavior available to a second execution. Preserve its settlement owner when introducing execution-local state. |
| `ResidentKernelBehavior` | Classify `active_workbench_control`, `fork_publication`, after-tool activity, pending replies/cancellations and route state by their actual consumers. Some describe model/request turns rather than notebook executions. Do not mechanically clone the whole behavior or move every field into a cell record. |
| After-tool cleanup | `run_after_tool` callers snapshot `parked_continuations` and use `abort_parked_since` on timeout. Replace session-wide subtraction with exact owned continuation cleanup before concurrent handlers can run. |
| External operations | Reuse the foundation's deferred work and parked continuation. Cancellation must signal supported operations and retain actual completion/cleanup evidence; removing the caller's waiter cannot settle the operation. |
| Admission identity | `WorkbenchCallKey` and `execution_id` already qualify invocation context with actor/incarnation. M1 must map the harness owner's agreed origin-operation identity to this boundary, including inherited claims; do not mint a fresh child operation for a replay. |
| Publication | Design one owner transition for staged declaration/binding changes and cancellation ordering. Invalid declaration joins must fail before visibility changes. Retain original binding identities and dependency leases. |
| Captures | Separate retained lexical meaning from mutable resource state. An independently retained successful capture must outlive later parent failure; this changes current boundary settlement behavior. |

Before M1 implementation, agree the bound endpoint/authority, immutable tool
manifest, origin-operation identity, retained result and cancellation contracts
with the harness owner. Before M2 implementation, review the field classification,
publication transition and capture lifetime together. Neither milestone requires
a second scheduler, completion registry or durable copy of live Haskell values.

- A raw `WorkbenchRequest` retains source and a transport-minted
  `WorkbenchExecutionId`; the authenticated `WorkbenchCallKey` includes actor
  incarnation, thread, turn, call and optional context-call/namespace identity.
  `WorkbenchExecutions` fences an unconfirmed call and returns the original
  terminal receipt on exact retry
  ([workbench.rs](../tidepool/runtime/src/session/workbench.rs),
  [workbench_ledger.rs](../exomonad/actor/src/resident_actor/workbench_ledger.rs),
  [resident_tools.rs](../exomonad/actor/src/resident_tools.rs)). Keep this
  identity as the key of each cell's state and settlement; do not mint another
  execution identity when a call wakes.
- `ResidentToolClient::dispatch_workbench` holds `dispatch_gate` through the
  whole call and keeps one `active_workbench` slot. The shared actor handle has
  one `HostedCellSlot`. Dropping the dispatch future drops
  `HostedCellPublication` and clears that *visibility* slot, but an already
  sent `KernelMessage::Workbench` still owns its request and control. The
  actor records and settles the control when the behavior finishes. A drop
  after send can leave the client's `active_workbench` slot stale because its
  ordinary clear is after the oneshot receive; this is not proof that the
  actor's continuation was aborted. `cancel_workbench` only claims the active
  call while it is in `WORKBENCH_SLEEPING`; a computing call returns
  `NotSleeping` ([resident_tools.rs](../exomonad/actor/src/resident_tools.rs)).
  Removal of the dispatch gate or a new claim protocol must retain exact
  settlement and clean up the per-call client record on every dropped future.
- `LocalActor::handle` awaits `behavior.workbench(...)` directly. That method
  borrows `&mut ResidentKernelBehavior` through `execute_workbench`, which
  awaits preparation, each effect and final settlement. Ractor cannot dispatch
  another cell, control input, reconciliation or a ready mailbox message while
  this handler awaits. The actor also holds one `active_workbench_control`,
  `fork_publication`, current posture and several mutable effect-routing fields
  ([local_actor.rs](../exomonad/actor/src/local_actor.rs),
  [resident_actor.rs](../exomonad/actor/src/resident_actor.rs)). The machine's
  short checkouts already release between suspended operations; the enclosing
  actor future is the scheduling blocker.
- An accepted call must become actor-owned state keyed by exact execution ID.
  Drive one bounded machine step per actor message, return from the handler
  whenever an external effect parks, and send its result back as an exact
  execution/continuation message. The actor then resumes that cell. Retain the
  original reply port, control, receipts, private scope and publication status
  in that cell record. External I/O futures may deliver messages; they must
  not mutate the actor's workbench state or become a second result registry.
  A stale effect response or cancellation for cell A must never consume cell
  B's continuation. The existing single `Sleep` control, fork boundary,
  request reservations and after-tool state need per-cell ownership or an
  explicit actor-global operation with a stated admission fence.

Current `Sleep` cancellation calls `abort_live` on the exact parked
`ResidentHole`, then acknowledges cancellation only if the hole was consumed
([resident_actor.rs](../exomonad/actor/src/resident_actor.rs),
[resident_workbench.rs](../exomonad/actor/src/resident_workbench.rs)). The
after-tool timeout separately calls `abort_parked_since`; that helper assumes
one handler runs at a time and finds holes by subtracting a global snapshot.
This assumption becomes false with concurrent cells. Replace that cleanup with
the slot's own continuation IDs before admitting concurrent handlers. A dropped
backend response must not silently drop an actor-owned parked continuation;
settle or abort it through its exact cell record and retain an unconfirmed
outcome when the result cannot be established.

## Capture, private execution and publication

1. At raw-cell admission, under one resident-session checkout, capture the
   actor's *published* lexical environment with
   `ResidentSession::mint_detached_scope(actor_scope)`. It freezes both the
   `SessionLib` declaration tip and the `BindingTable` immutable value tip;
   detached scope leases also retain shadowed ancestor value generations used
   by old declaration modules
   ([persistent.rs](../tidepool/runtime/src/session/persistent.rs),
   [binding_table.rs](../tidepool/codegen/src/binding_table.rs)). Record the
   exact source-layer revision and tool surface selected for this call. Build
   the cell's `ActorSessionContext` with this private lexical scope while
   retaining its actor principal, machine session and runtime resource scope.
   `ActorSessionContext::compile_view` must still verify the exact scope
   ([mount.rs](../exomonad/actor/src/mount.rs)). New public commits after this
   checkout cannot change the captured meaning.
2. Prepare and execute all items against that private context. The existing
   `prepare_cell`, `begin_prepared_cell_item`, `settle_item` and effect-resume
   paths take the context already; today the actor passes its published scope
   and commits each successful item immediately
   ([resident_workbench.rs](../exomonad/actor/src/resident_workbench.rs),
   [resident_actor.rs](../exomonad/actor/src/resident_actor.rs)). Retain
   compiler-owned declaration receipts, private declaration generations,
   actual binding identities and final replacements/retractions as a
   cell-local write set. A set difference of rendered names is insufficient:
   rebinding an inherited name, constructor/class exports and instance-only
   declarations would be lost. An expression's generated observation binding
   follows the same explicit final-binding policy as other new bindings.
3. A successful cell publishes its *final new* declaration/binding view in one
   completion-order operation owned by `PersistentSession`/`SessionLib` and
   `BindingTable`, reached through `ResidentSession`. Stage every fallible
   compiler/file operation first, then perform one checkout-scoped visibility
   change. No public name or declaration tip becomes visible if staging fails.
   Publication receives the cell's captured base and private write set; it
   never changes the public tip to the private tip or recompiles private source
   against a later public environment. Another cell's unrelated public names
   remain visible. Same-name writes follow **publication completion order**,
   including when the winning cell compiled at an older generation.
4. On failure/cancellation, retire the private scope after independently owned
   checkpoints are detached. Publish no notebook definitions. Report the
   actual completed effect and command receipts; already committed external
   effects and explicit source reloads retain their own owner-defined
   semantics. Release only this cell's parked continuations, leases and roots.

### Binding publication

Reuse the private binding's **original** `SessionVarId`, `Val.G` interface and
root. `BindingTable::live` is globally keyed by ID, but its immutable
`BindingTip::visible` already maps names to IDs owned by another scope;
`seed_scope` acquires leases instead of copying entries. A retired private
owner keeps leased IDs live, so later compilation can import the same typed
interface ([binding_table.rs](../tidepool/codegen/src/binding_table.rs)).
Fresh typed aliases would add a GHC compile per binding, churn identities and
provenance, and still need private-source dependency retention. They are not
needed for a same-name notebook export.

Add a runtime-owned publication of a merged actor binding tip: start from the
actor's *current effective* visible name-to-ID map, overwrite only the cell's
final binding names in **completion order**, and acquire all new visible and
declaration-dependency leases before releasing the old tip. This explicitly
chooses the winning ID; it must not call ordinary `bind_in`, which chooses the
highest **compile generation** and would let an earlier-completing cell beat a
later one. Atomically pair the tip swap with declaration retractions that hide
a replaced declaration. Do not mutate `live` or `BindingIndex` when merely
publishing an existing ID; both already track that entry and its module while
the acquired lease keeps it live.

The actor scope is currently an isolated root with no seeded tip, and its
mutable `current` frame takes precedence over a tip. The publication owner
must normalize the effective actor view when first enabling this path and
remove/reconcile any old local-frame entry for a newly published name; otherwise
the old frame silently masks the new tip. Later host mounts that legitimately
write the actor scope need the same ordering rule or an explicit private
carrier path. `remove_live`, observation expiry and host-text lookup presently
use `BindingEntry.scope` as the owning frame; a published tip does not change
that owner. Preserve owner cleanup, but make any public *visibility* query use
the actor tip's chosen ID rather than assuming the visible entry was born in
the actor scope. Published automatic observations need their existing bounded
retention policy applied to the public view, rather than accumulating one
recent observation per retired private scope.

The tip must also retain dependencies of imported declaration modules, not
only currently visible names. At baseline `b2de366b2`, `seed_detached_scope`
captures visible IDs and scans source/ancestor owner frames, so it misses a
shadowed ID owned by a retired private scope and retained only in the source
tip. The same bug affects `retain_scope_dependencies` when importing a
detached source. Commit `41583b5eb` fixes transitive retention in this existing owner. Three
focused regressions pass: capture of a capture, importing a hidden dependency,
and avoiding duplicate leases when importing an alias. Each reproduced its
respective defect before the fix. `scope_reachable_modules` already
includes `tip.retained` for compiler source readiness once those leases are
present. The binding-tip publication and concurrent cell changes remain open.

### Declaration publication

`DeclTurn` has one parent generation; `render_module_with_vals` imports and
re-exports that parent plus the turn's own source
([render.rs](../tidepool/runtime/src/session/render.rs)). The private tip
contains its admission snapshot, so assigning it to the public scope would
erase intervening public commits. Replaying private text against the new
public tip may resolve names to different values or types. Extend `SessionLib`
with a compiler-validated *join* generation: its ordinary parent is the
current public tip, and its separate exact import names only the private
cell's final declaration exports. Hide replaced public heads using GHC's
`ExportItem` structure, including constructors and class methods. Preserve
the private modules and all value leases they require for the lifetime of
that join and later compiled code. A new public generation gives later
admissions the merged view while already admitted cells continue to import
their captured generations.

The join's delta is not just exported heads. Track private `DeclTurn`s with
instance declarations even when their `ExportItem` set is empty; GHC must
validate their instance visibility and conflicts in the joined environment.
Carry normalized authored imports/prologues needed by those declarations,
without importing the private cell's entire captured namespace as newly
public. Track declaration retractions caused by a later private materialized
binding (`x` declared then bound) and by replacement of a captured public
declaration. The final public view must export the final binding or declaration
once, not both. If selective import of a private module pulls an unwanted
instance or hidden captured name into the public interface, reject that
candidate or extend the declaration representation; a name-list-only join
would silently violate the accepted isolation contract. Use the existing
staged-declaration validation/adoption discipline as the model for write and
tip rollback ([persistent.rs](../tidepool/runtime/src/session/persistent.rs),
[mod.rs](../tidepool/runtime/src/session/mod.rs)).

## Immediate checkpoint boundary

The current `ForkGroupBoundary::Checkpoint` handler calls
`capture_context_scope(context)` during the effect, which freezes the
context's lexical scope and source-layer identities before returning its token
([resident_actor.rs](../exomonad/actor/src/resident_actor.rs),
[resident_workbench.rs](../exomonad/actor/src/resident_workbench.rs)). With a
private cell context, this captures the declarations and bindings of completed
earlier items immediately, even if that cell is still suspended. Function-local
values still cross only as explicit typed inputs. Record the checkpoint as an
independent completed fact and keep its detached scope/source leases until its
own release. The current end-of-workbench failure path calls
`settle_checkpoints(..., false)` and retires checkpoints from that boundary;
change it so failure of the enclosing cell does not revoke a checkpoint that
already returned successfully. A checkpoint attempted but not committed may
still be cleaned up. Its harness conversation prefix and pending-call claims
must refer to the same effect boundary and original provider identities.

## Ordered owner changes and discriminating checks

1. **Runtime stores:** first fix transitive detached-tip dependency retention;
   then add the staged public declaration join and merged binding-tip
   publication, plus a single final-delta commit boundary. Test
   private capture versus a concurrent public write, two different-name
   publications, and two same-name publications in *reverse compile order*.
   Assert old cells resolve old values and the public view follows completion
   order. Test `data` constructors, class methods, instance-only turns,
   normalized imports, declaration-to-binding retraction and public
   declaration/value shadowing and a checkpoint whose hidden imported value
   outlives both earlier scopes. A test that only compares final name strings
   would miss interface, instance and root-lifetime failures.
2. **Actor cell state:** mint the private context before preflight; keep
   per-execution request/control/reply/receipts/continuation ownership. Test
   a later item failure after private declarations and binds: no public
   exports, retained effects and commands, exact private-root cleanup. Test
   a checkpoint returned after a completed private item, a child using it
   before parent completion, and parent failure afterward. The checkpoint
   must retain its old meaning after another cell publishes the same name.
3. **Kernel scheduling and cancellation:** replace the full-cell awaited
   `KernelMessage::Workbench` handler with bounded step/result messages;
   remove the tool-client `dispatch_gate` only after per-call controls and
   actor fields are in place. Test two independently suspended cells while a
   third cell and a control input progress, then wake in reverse order. Test
   cancellation of one exact sleep, a computing call's `NotSleeping`, late
   stale effect replies, host dispatch-future drop after actor admission,
   exact retry, and actor retirement. Neither a sibling continuation nor a
   successful checkpoint may be consumed by cleanup.
4. **Harness adapter dependency:** consume the accepted raw/typed call and
   claim API once it supplies stable original call identity, admission,
   cancellation acknowledgment, pending-call checkpoint capture and exact
   output routing. Current actor/runtime primitives can be designed and
   tested without that adapter. End-to-end admission, backend cancellation
   and restored pending-call correlation cannot be asserted until the
   harness API revision is fixed. The adapter must route a claimed call to
   the exact actor incarnation and return the retained result without
   reexecuting source.

No builds or tests were run for this read-only investigation and plan write.
