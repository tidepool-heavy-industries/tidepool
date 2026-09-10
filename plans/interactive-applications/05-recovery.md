# Reconnection, recovery and operator behavior

Behavioral contract for the retained implementation. See [source inventory](current-state-review.md)
and [wrap-up checklist](06-integration.md) for current status and assignments.

## Result

The operator can distinguish a broken connection, disabled hosted coordination,
a stopped native application and a lost Shoal host. Each has one permitted next
action. No automatic recovery creates a competing executor, replays uncertain
steering or pretends a new resident machine contains old Haskell values.

## Four different operations

| Operation | Preserved | Must be re-established |
|---|---|---|
| Reconnect host transport to the same live native application | Actor, request registry, resident machine, native execution and input producer | Fresh endpoint/generation handshake and missing observations |
| Refresh the primary native session handle within the same TUI | The explicitly retained actor and application identity | New session generation, admission gate and exact pending-operation status |
| Resume a retained conversation into a new actor incarnation | Native durable history and available source/artifacts | New actor/request authority, resident machine, scope, binding, input producer and readiness |
| Start a fresh actor from selected context | Only the context and resources explicitly selected by the normal launch owners | All live execution and coordination state |

Attaching tmux to a pane is a UI operation. It performs none of the authority
transfers above. Archive/history APIs are also separate from stopping execution.

## Failure policy

| Observation | Automatic behavior | Operator-visible result |
|---|---|---|
| Brief relay disconnect, native helper still running | Suspend input; reconnect to the same application; query uncertain operations | Reconnecting with original TUI available |
| Hosted completion retries exhausted | Disable hosted coordination for that actor incarnation; retain native scope | Unavailable hosted tools; ordinary native conversation remains usable |
| Native provider turn fails while application remains live | Report native failure through existing observations; preserve TUI and request authority | Provider failure, not a declaration of process death |
| Native application exits, helper retains exact scope | Finish scope accounting and actor failure/retirement | Native stopped or cleanup pending, with outstanding hosted work shown |
| Tmux pane disappears but scope is live or unknown | Retain obligations; inspect the original helper | Pane unavailable; native execution not assumed stopped |
| Supervisor disappears | Retain unconfirmed process/resource custody | No automatic helper adoption or competing resume |
| Shoal host process dies | Keep released TUIs and supervisors alive; disable hosted coordination when detected | Retained applications with lost coordination, not recovered actors |
| Intentional actor stop | Run the full retirement transaction against its original owners | Stopped with confirmed settlement, or explicit retained obligations |

The host health loop consumes separate native, helper, hosted endpoint and actor
observations. Timeouts expire observations, not identities. Do not make a single
`healthy = pane_exists && http_ok` predicate control recovery.

## Reconnecting while the original host survives

The existing deployment row remains authoritative. Keep its request registry,
resident machine, accepted hosted work and resource leases alive while transport
reconnects. Retry read-only handshake/status with bounded backoff. Do not allocate
another actor, inbox, process scope or producer.

On reconnect:

1. Challenge the original endpoint and require the exact launch and application.
2. Acquire the current session generation and coordination state. A new TUI
   instance or fatally disabled coordination cannot be treated as a successful
   transient reconnect.
3. Reconcile all in-flight/unconfirmed input IDs using native durable outcomes.
   Missing evidence retains uncertainty; a negative result requires the exact-key
   withdrawal fence specified in the delivery plan.
4. Reconcile accepted hosted calls and persisted completion callbacks through
   their original owners. Do not rerun Haskell or repeat a child admission.
5. Refresh native turn and usage snapshots, then resume bounded observation.
6. Resume automated input only when the producer is open, required capabilities
   still match and the ordered delivery head is eligible.

If the native handle was replaced inside the same TUI, old accepted operation
results remain associated with their original generation. Reconnect permits
queries and subsequent new operations on the current generation; it never rewrites
the generation that may already have accepted an input.

## Losing the Shoal host

The current `HostIncarnationLease` file lock and durable incarnation allocation
remain the owner of run-root exclusivity. A successor must acquire that lock;
it must not decide the old host is dead merely because status publication is
stale. The new incarnation cannot authorize old `ActorRef` values.

Persist only the minimum recovery description in the existing run/deployment
records: old actor/launch identity, conversation locator, helper endpoint,
installed worktree/build/socket identities, frozen selection identity and last
observed outcome. Use existing durable-write and versioning mechanisms. Do not
persist a second request registry or serialize live capabilities.

A successor can contact a surviving supervisor using the exact immutable launch
record and a fresh authenticated local challenge. Under exclusive run ownership,
the helper can grant a **retirement-only** connection: inspect or stop its one
existing scope. It cannot release an unreleased payload for a different host,
change its command, restore actor coordination or accept a new producer. The
helper fences stale control connections when transferring this limited authority.
This is process recovery, not actor recovery.

The native host bridge never binds old calls to the successor's Haskell machine.
Old hosted calls fail with coordination unavailable. Old host input remains
sealed/quarantined. A callback bearing an old incarnation cannot acknowledge a
new request, launch a child or select the successor's actor by numeric ID alone.

### Worktree custody limitation

`tidepool-worktree/src/binding.rs` deliberately does not deserialize
`ActiveBinding` receipts. Its bind generations are process-local, and a reopened
active row remains busy. Preserve that contract in this implementation.

A surviving supervisor can prove native process completion after host death. It
cannot recreate the lost host receipt or prove that every host-side accepted
effect stopped. Therefore whole-host crash recovery may leave the old worktree,
build or socket obligation retained even after the native scope is stopped.
Report these as separate facts, with the old binding and owner identity.

Do not add `release(worktree_id, cleanup_bool)`, manufacture an `ActiveBinding`
from its serialized row, rewrite a binding to released, or accept an arbitrary
process receipt to work around this. Automatic settlement after loss of all
original host custody would require a separate durable authority design for
every participating resource owner; it is outside option A's recovery guarantee.
The ordinary retained-host retirement path in A5 must still settle successfully.

When an old binding cannot be settled, a new run uses a fresh managed checkout
under existing placement policy, or reports that the requested original checkout
is still busy. It must not start a second writer in the retained checkout. The
operator can inspect preserved work and choose a fresh seed through normal
worktree operations. This is a visible limitation, not an automatic repair that
deletes old evidence.

## New-incarnation conversation resume

Remove pane death as sufficient authorization for the current root resume path.
Before opening a new executor for the same conversation, require exact old native
scope termination from its surviving owner. For an old legacy launch or missing
helper with no valid proof, refuse automatic same-conversation resume. Offer
inspection and a fresh conversation through existing context selection instead.

A new-incarnation resume proceeds as follows:

1. Establish that no old native execution can continue on that conversation.
   Finish the old retirement where its original owners survive; otherwise retain
   any separately unprovable resource obligations as described above.
2. Obtain a valid checkout binding for the successor. Reuse is allowed only if
   the existing binding owner actually settled the predecessor; otherwise use an
   independently allocated checkout or stop with busy custody.
3. Allocate the new actor incarnation, inbox producer, scope and hosted endpoint
   through the normal launch transaction. Old request handles and receipt
   authority remain stale.
4. Open native durable history using the existing explicit local resume command.
   Quarantine old host queue records; do not replay their assignments or updates.
5. Initialize the new resident machine from frozen configured modules and any
   supported declaration source recovery. Do not evaluate old tool calls from
   the transcript as a bootstrap script.
6. Publish a bounded recovery notice and a new explicit task/bootstrap through
   the normal input path after readiness. Explain that old resident handles and
   pending response obligations were not restored. Preserve the shared frozen
   prompt prefix; append recovery facts as new task data.

Use `tidepool-runtime/src/session/recovery.rs` and its
`DeclarationRecoveryReport`. Replay only the ordinary declarations it marks
replayable, validating recorded source hashes. Report replayed and lost source.
Closures, evaluated values, actor handles, effect resources, request replies,
watch callbacks and arbitrary machine heap contents remain tied to the old
machine. A successful compilation of recovered declarations is not a recovered
actor session.

Retained transcript messages mentioning old Haskell bindings are history, not a
grant to make those bindings valid. Model-facing failures for stale handles must
remain small and useful. Do not silently reconstruct live handles by matching
names or IDs from transcript text.

## Operator actions and status

Extend the existing Shoal CLI/operator service and status projections, with
typed actions implemented by the existing lifecycle owner. Keep command parsing
in `tidepool/src/bin/shoal.rs` and composition in `shoal.rs`/`actor_host.rs`.
Do not add a new daemon, dashboard, terminal input language or scheduler.

Expose these concrete operations through that existing command surface:

| Action | Behavior |
|---|---|
| Inspect application | Show actor/request, native, coordination, scope and resource state with observation provenance |
| Attach pane | Attach the existing native terminal; no new execution |
| Reconcile connection/input | Query the retained live binding and exact operation IDs; no resubmission |
| Retire hosted coordination while preserving native use | Seal actor/input admission, terminate typed obligations through actor failure, quiesce hosted work and retain native custody |
| Stop retained application | Use the original scope or limited recovery connection, then report each cleanup obligation |
| Resume retained conversation | Perform the gated new-incarnation procedure; never imply restored live actor state |

Reuse existing commands where semantics fit; add explicit variants to the current
CLI/operator enums for missing actions. Existing user-requested run replacement
and stop paths must call these same owners. They cannot retain a separate
kill-pane implementation. Internal helper control is not directly exposed as a
model-facing Haskell operation.

The default inspect view should answer: what work is assigned, whether native
execution is running, whether Haskell coordination is available, whether an input
is uncertain, and what is preventing cleanup. Detailed/JSON output additionally
includes exact identities, generations, revisions and outstanding owner names.
Do not present model-generated prose as confirmed incorporation; show the
request's actual response/evidence separately from delivery status.

## Acceptance

- [ ] Transient disconnection preserves the same actor, request registry, Haskell values and native application.
- [ ] A fresh handshake rejects stale paths and a replacement application.
- [ ] Input and completion reconciliation after reconnect performs no fresh evaluation or uncertain resend.
- [ ] Human interaction works in a retained TUI after hosted coordination is disabled.
- [ ] Original-host retirement can later stop that same retained application and settle its leases.
- [ ] A successor cannot start until it acquires the existing run-root ownership lock.
- [ ] After host death, limited helper recovery can stop only the exact old scope and cannot restore hosted authority.
- [ ] Old `ActorRef`, request, callback and notification receipt identities are rejected in the successor.
- [ ] A reopened active worktree binding remains busy; process evidence alone cannot forge its receipt.
- [ ] Unknown old execution blocks automatic same-conversation resume.
- [ ] A valid successor uses a new producer and never drains old host queue items automatically.
- [ ] Declaration recovery reports replayable source and lost live state; effectful calls are not replayed.
- [ ] Native exit, provider failure, dead pane and broken hosted HTTP each produce their distinct status/action path.
- [ ] Existing stop/recreate entry points use the same retirement mechanism as explicit retained-application stop.

Whole-host crash tests must assert both what can be recovered and which custody
remains retained. Reporting only that a new TUI appeared is insufficient.
