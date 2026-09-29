# Concurrent resident notebook: runtime handoff

Proposed engineering design, 2026-09-28, traced against baseline `b2de366b2`.
**None of the concurrency or publication changes below are implemented.**
The accepted behavior is in
[`harness-integration.md`](harness-integration.md); this handoff identifies the
current owners and the smallest source changes that can preserve capture identity
and completion-order publication. It does not authorize replay of effects or
restoration of live heap values after host loss.

The joined foundation includes per-execution hosted-call tracking and transitive
binding retention (`41583b5eb`, joined by `9bdec2912`). The analysis below is
reconciled against the joined candidate `11c96dc0e`. It does not imply that
the foundation admits concurrent workbench executions.

## Baseline execution and the blocking seams

### Foundation reconciliation — joined candidate `11c96dc0e`

The first-tree PRD now separates sequential embedding (M1) from concurrent
private executions and reusable captures (M2). This inventory is design input,
not authorization to enable concurrency before foundation acceptance and the
per-execution ownership review. The source descriptions below predate active
foundation candidates; reconcile them against the accepted integration revision.

| Seam | Preparation / required decision |
| --- | --- |
| `LocalActor` and hosted-call visibility | The joined foundation moves one owned behavior into an active task and retains accepted controls independently of caller futures. This makes control responsive; it does not make the behavior available to a second execution. Preserve its settlement owner when introducing execution-local state. |
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
- `ResidentToolClient::dispatch_workbench` still holds `dispatch_gate` through
  the call. It publishes an exact `WorkbenchExecutionControl` before the gate;
  the shared `HostedCellSlot` now retains multiple controls. The gate also
  serializes ordinary `Tool` calls and direct untracked workbenches, so its
  removal needs an explicit admission rule for those operations. A dropped
  caller future does not own or cancel an actor-admitted call. Sleep cancellation
  still requires `WORKBENCH_SLEEPING`; a computing call returns `NotSleeping`
  ([resident_tools.rs](../exomonad/actor/src/resident_tools.rs)).
- `LocalActor` owns one `BehaviorSlot` and one `pending_workbench`. It moves
  the **whole** behavior into a spawned call task, retaining the reply/control
  until completion. Its Ractor handler can seal admission, defer shutdown and
  answer some reconciliation messages, but defers unrelated mailbox work and
  child-exit handling until the behavior returns. This is responsive control,
  not execution interleaving ([local_actor.rs](../exomonad/actor/src/local_actor.rs)).
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

### M2 ownership and minimal transitions

| Owner | Retained state and transition |
| --- | --- |
| `LocalActor` / Ractor | Exact-incarnation admission, lifecycle, mailbox order and a map of admitted workbench executions. Admit a call once, then schedule bounded machine steps and exact completion messages. Never move the whole `ResidentKernelBehavior` into a long-running cell task. |
| Actor's resident behavior | Descriptor, installed policy and reload generation, model-turn `standing`/checkpoint/input, request reply and cancellation settlement, source connections, roster snapshot and aggregate observation. Ordinary synchronous mailbox turns remain non-reentrant. |
| One workbench execution | Original request/reply/control, immutable admitted source and tool leases, captured declaration/binding tips, private scope and final write set, current continuation identity, effect receipts, fork provenance, after-tool recursion state and publication decision. The same execution identity survives every park and wake. |
| Route operation | `active_route` and `ForkPublication::Route` belong to the exact watch callback, not to any concurrently admitted cell. Boundary-indexed pending fork publications remain actor-retained until published or explicitly reconciled. |
| `WorkbenchExecutions` | Existing exact-call replay and terminal receipt authority. A retry observes the original result or an unconfirmed fence; it never reruns source or effects. |
| Resident session | Machine checkout, declaration join validation, binding leases and the one atomic public tip swap. The checkout serializes sibling commits; it does not schedule actor work. |

An admitted call first captures the current published environment and admitted
source/tool versions under one checkout. A machine step may return a completed
result or an exact parked continuation. On park, the actor records that hole in
the execution and returns to Ractor. An external operation owns only its own
payload and sends an `(execution, continuation generation, result)` completion;
the actor checks both identities before resuming. Retirement or a dropped client
waiter cannot turn uncertain external work into a clean cancellation.

`WorkbenchExecutionControl` should add a typed per-execution decision state for
publication (`Running`, `CancellationRequested`, `Published`, terminal without
publication) under one short lock. Its existing sleep atomics and terminal
watch do not establish this order. The actor calls
`control.commit_if_allowed(|| session.commit_prepared_delta(...))` **inside**
the machine checkout. `ResidentSession` owns a synchronous, no-await commit of
an already validated declaration/binding delta and has no dependency on the
actor crate. The control lock spans the visibility swap and records `Published`
before release. A cancellation that takes that lock first forbids publication;
one arriving after `Published` cannot revoke the commit. Preparing a compiler
join or waiting for resources never holds this lock. If another execution has
advanced the public generation during preparation, restage and revalidate the
join against that generation without rerunning the original cell or its effects.
Cancellation intent and confirmed cleanup are separate retained observations:
the exact continuation, command or request owner must report whether its work
actually stopped. An unconfirmed external operation keeps its cleanup authority.

Two actor-wide cleanup paths need exact operation ownership before interleaving.
`requests.abort_unsubmitted(actor)` currently removes **every** reserved request
for that incarnation, including a sibling cell's two-step request admission;
the rejected-workbench and route-finish paths both call it. Attach the creating
execution or route identity to the reservation and abort only that operation's
unsubmitted IDs; actor retirement may still sweep the whole incarnation. The
after-tool timeout currently subtracts a snapshot of all parked machine holes.
Record the slot's exact continuation IDs and abort those IDs only. Keep its
ordinal/log actor-owned while recursion suppression and parked holes belong to
the slot invocation. Publish per-execution posture into an actor aggregate so
one completed cell cannot display `Idle` over a sleeping sibling.

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
with an explicit `DeclNode::Join` (or equivalent typed variant), rather than
pretending that a join is another authored `DeclTurn`. Its ordinary parent is
the **current** public tip; a separate exact private generation supplies only
the cell's final declaration exports. Record original export provenance, GHC
`ExportItem`s, normalized imports/prologues, instance modules and retractions
in the join receipt. Hide replaced public heads using `ExportItem`, including
constructors and class methods. Retain the original private modules and their
value/source leases for the lifetime of the join and later compiled code; do
not mint new `SessionVarId`s or recompile private source against a changed
public environment. Later admissions see the merged generation while already
admitted cells keep their captured meaning.

The join's delta is not just exported heads. Track private `DeclTurn`s with
instance declarations even when their `ExportItem` set is empty. A selective
name import can omit their binders while still carrying instances. The
compiler proof found that `import M ()` carries instances in GHC 9.12.2.
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

The focused `declaration-join-proof` compiler suite exercises hand-written
wrappers corresponding to those imports through the resident checked-environment
pipeline. Its six join/consumer pairs distinguish acceptance at the proposed
commit gate from behavior in a later cell:

| Case | Joined wrapper | Later consumer | Consequence |
| --- | --- | --- | --- |
| Private hidden name | accepted | hidden-name use rejected | Explicit exports preserve name isolation. |
| Same-name private replacement with a different type | accepted | private `Int` plus unrelated public `Int` accepted | Selective hiding chooses the private origin and preserves unrelated public names. |
| Two distinct instance-only modules imported with `()` | accepted | both instances usable | An empty name import still carries instances. |
| Duplicate `instance C Int` from two valid modules | **accepted** | overlapping-instance use rejected | Compiling the wrapper alone is insufficient to validate publication. |
| Two unrelated classes named `C`, both exported | rejected | rejected | GHC catches ambiguous class exports at the wrapper. |
| Conflicting `type instance F Int` | rejected | rejected | GHC catches this family conflict at the wrapper. |

`validate_rendered_module` currently asks GHC for each term export's type, or
for `()` when there are no term exports. The duplicate-class-instance
counterexample passes that validation shape and fails only when a later cell
uses the class. M2 therefore needs a worker-owned check of the **combined
imported instance environment**, including heads reachable only through
`M ()`, before the staged join becomes visible. Wrapper compilation remains
required for names, exports, families and normalized imports; it is not the
whole validation. The worker should return a typed result for the exact staged
join. If GHC cannot prove a particular combination safe, publication fails
atomically with a declaration-join diagnostic, while the cell's already
performed external effects and independently retained captures remain intact.

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
own release. The checkpoint must retain the **admitted** source-layer version
and a strong lease on its immutable revision paths, rather than freezing the
actor's mutable active source when the effect happens later. Today
`capture_context_scope` freezes the lexical scope while the adjacent
`freeze_checkpoint_layer` call rereads active source identities; the before/
after comparison only detects a change during that capture, not a source
reload between cell admission and capture. The current end-of-workbench failure
path calls
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

The declaration-join proof is a compiler experiment, not an implementation of
private execution or publication. Its exact command and retained output are
reported with the candidate commit; the remaining M2 checks above have not
run on a concurrent resident implementation.
