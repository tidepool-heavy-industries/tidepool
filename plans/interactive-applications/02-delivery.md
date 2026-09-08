# Durable input and delivery outcomes

Status: planned. Implements slices A2 and A3 after the
[live session contract](01-native-session.md).

## Result and guarantee

Each host-originated input has a stable identity, an immutable payload and an
explicit delivery mode from publication through native presentation. A lost
acknowledgment cannot create a second assignment or duplicate steering. An
unprovable outcome remains unconfirmed and has a defined resolution path.

The guarantee is durable deduplication of native admission, with conservative
handling of the separate engine/history boundary. It is not exactly-once model
execution, exactly-once tools, or proof that the recipient incorporated a message.
Crashes between stores must be represented, not hidden by that terminology.

## Existing owners and missing behavior

The sole host queue is `tidepool-node/src/inbox.rs`. It already owns durable rows,
checkpoints, receipt provenance, pre-send attempts and restart reconciliation.
Extend it. The host's current `deliver_pending` prefix batching and
`backend.push` consumer are replaced for Shoal once this path is wired.

On the Codex side, extend the existing queue and history owners:

- `codex-rs/ext/queue/src/service.rs` owns native queue admission and dispatch.
- `codex-rs/thread-store/src/queue_store.rs` owns the storage-neutral queue seam.
- `codex-rs/state/src/runtime/queued_items.rs` and state migrations own local
  queue persistence.
- The existing native session input and history append paths own when user input
  becomes part of the model-visible conversation.

Today the queue service accepts a `client_id`, but does not use it as a durable
deduplication key. Dispatch removes the queue row after `start_turn_if_idle`.
Consequently neither a stable CLI argument nor searching the pending queue alone
solves acknowledgment loss or the crash between dispatch and deletion.

Add input admission metadata and outcome retention to these existing native
stores. Do not add a second Shoal delivery log, a TUI-only dedup cache or a new
general-purpose effect execution journal.

## One envelope per operation

Publish exactly one input operation per inbox sequence. Stop rendering a changing
batch from every currently pending row. If batching is later justified by measured
cost, it must be an explicit immutable envelope with its own identity; it is not
part of this implementation.

The envelope preserves:

| Field | Rule |
|---|---|
| Producer | Existing run/inbox identity and exact actor incarnation; installed by the host, never supplied as authority by message text |
| Sequence | Allocated by `DurableInbox` and never reused |
| Purpose | Typed `Bootstrap`, `Assignment`, `RequestUpdate`, `Notification` or an existing ordinary operator-input purpose |
| Mode | Typed queue-only or start-or-steer; fixed at publication |
| Target | Exact actor and retained conversation, plus the live binding required for this attempt |
| Correlation | Existing request/update ID or notification provenance when applicable |
| Payload | Validated, bounded input with rendered bytes frozen once before native submission |
| Content digest | Digest of the canonical mode, target and payload; excludes transport generation and attempt timestamps |

Use the inbox producer and sequence as the native `client_user_message_id`, with
a documented bounded encoding. Do not allocate another UUID in the CLI, backend,
retry loop or request update owner. Human-originated native input keeps its native
identity mechanism and cannot impersonate a host producer.

Keep source payload types in their current owners. The node inbox stores generic
typed receipt/delivery metadata; it does not import Codex or actor request policy.
Persist the rendered operation, or an immutable versioned representation that
reproduces exactly those bytes. A changed renderer after restart must not silently
change a previously admitted payload. A repeated key with different canonical
content is a conflict, never a replacement.

Bootstrap moves from Shoal's positional native launch argument to this envelope.
It uses the same frozen initial-message bytes after native readiness. Assignment
publication still follows the actor driver's existing request admission and
mounting rules. The transport does not manufacture `sessionInput` or `sessionReply`.

## Delivery modes and request semantics

| Purpose | Native operation | Actor effect |
|---|---|---|
| Bootstrap or admitted ordinary assignment | Queue for native idle dispatch | Existing actor owner creates and mounts the intended work |
| Update to an existing presented request | Start-or-steer through the owning session | Acquire the existing request presentation lease; original reply obligation remains pending |
| Notification | Start-or-steer through the owning session | No request creation, replacement, reply capability or request-settlement fence |
| Ordinary queued operator follow-up | Queue for native idle dispatch | Preserve its existing actor/operator semantics |

For an update, **active means an existing typed request**, not necessarily a native
turn currently generating tokens. The native session may be idle while the request
remains open. Start-or-steer can wake that same work or steer its running turn.
It cannot fall back to an ordinary queued assignment after a routing failure.
Request settlement racing update admission is decided atomically by the existing
`RequestRegistry` presentation lease, before native submission.

Human input can change native turn state. Native start-or-steer makes the native
decision atomically; a host-side `is_idle` check followed by an unrelated request
is not sufficient. The backend does not infer which typed request exists from
turn IDs, text or provider status.

## Native admission and storage

Use one host producer for the bound primary thread. Installing or replacing it
requires the exact live binding. Persist its accepted sequence/outcome metadata in
the existing thread store. A sealed or foreign producer cannot submit, even if it
knows a historical thread ID.

Admission performs validation and the following transaction before any engine
handoff:

1. Verify the live application/generation and open producer gate.
2. Look up the exact producer/sequence. Return its existing outcome if content
   matches; return a typed conflict otherwise.
3. Reject keys at or below the producer's compacted terminal watermark without
   creating another input. `AlreadySettled` is not a fresh presentation receipt.
4. Reserve the operation and queue payload atomically in the native store. Native
   queue-only inputs become ready; start-or-steer inputs are ready for that
   specific native mode. The two modes cannot be interchanged on retry.
5. Return accepted only after persistence succeeds. A failed write is never
   reported as accepted, and an uncertain write poisons mutation until reconciled
   through the storage owner.

Dispatch claims a ready record into a durable `Dispatching` state before entering
the engine. The native session preserves its client ID through the actual user
input append. The existing history owner supplies correlated presentation
evidence after that append reaches its durable model-visible boundary. Merely
storing a queued item, receiving an RPC or writing a trace line is insufficient.

History persistence and queue state may use separate transactions. Do not pretend
they are atomic. If the process dies after the dispatch claim, recovery searches
the canonical native history for the exact operation ID. A matching durable input
can establish presentation. Absence alone cannot prove the input was never
accepted by the engine. An unresolved `Dispatching` record becomes unconfirmed;
it must never return to the ready queue automatically.

This rule must cover both explicit queue `start` and ordinary idle dispatch for
host-owned records. Existing human queue entries retain their documented native
behavior; do not accidentally apply Shoal producer authority to ordinary users.

Add an exact-input withdrawal operation to the relay:
`POST /v1/input/withdraw`. The native owner atomically either removes/fences a
not-yet-dispatched operation and returns never-presented evidence, returns an
already-presented result, or reports that dispatch may already have happened.
For a previously unseen key, withdrawal records a tombstone so a delayed submit
cannot arrive after a negative result. An ordinary status query's `not found`
is not this fence and cannot clear an uncertain request.

## Native outcome lifetime and bounds

Retain admitted outcomes after queue consumption. Keep at most 256 unacknowledged
host input records per producer, additionally respecting the native queue's
existing capacity and input size limits. Admission at capacity returns typed
backpressure before submission. Do not evict an unresolved record to admit work.
The host input queue can remain durable while waiting for capacity.

The host acknowledges a contiguous prefix only after terminal observations are
durable in its inbox and any necessary request correlation has been incorporated
by its owner. The native store validates that acknowledgment does not skip its
own unresolved admitted operations. It can then compact terminal records into a
producer watermark. Replays below the watermark cannot reenter the engine, even
when detailed outcome evidence is no longer retained.

Closing a producer fences all later submissions and quarantines undispatched
items. A new actor incarnation uses a new producer. It cannot reopen old inputs.
Keep the current producer and its seal/watermark in the existing thread metadata;
do not accumulate an unbounded active-producer registry. Historical producer
queries after compaction or replacement may return evidence unavailable. Foreign
or retired producer IDs always fail admission, including after a native restart.

On resume, old host-owned queue entries stay quarantined until the recovery policy
has accounted for the previous producer. They must not automatically execute in a
new actor. The TUI displays their origin and state. A person may delete a pending
host item, producing a correlated withdrawn outcome. Editing it must withdraw the
host operation and create ordinary manual input with a new native identity;
silently changing the host digest is forbidden. Manually replaying quarantined
text similarly creates new manual input, not a claimed successful old update.

## Host inbox transitions

Extend the existing tracked delivery mechanism. Preserve its single-open-owner,
strict directory durability, pre-send persistence and poison-on-uncertainty rules.
Do not weaken syscall failures to best-effort delivery.

| Host state | Trigger | Allowed next action |
|---|---|---|
| Accepted locally | Durable publication | Claim the next ready row |
| In flight | Durable pre-send fence acquired by the sole dispatcher | Submit once using the fixed key and payload |
| Not submitted | Definitive attempt rejection before admission, with no earlier uncertain attempt | Retry the same operation if still valid, or close it with explicit negative disposition |
| Submitted | Native durable admission confirmed | Advance accepted-delivery evidence; query presentation as needed |
| Presented | Exact durable native input evidence | Persist receipt and settle the appropriate presentation lease |
| Withdrawn / rejected before presentation | Native negative result excludes later dispatch | Persist terminal negative disposition; release only the corresponding presentation fence |
| Unconfirmed | Lost acknowledgment, dropped attempt or uncertain native dispatch | Query or withdraw the same key; do not send again |

The current inbox already supports late `confirm_presented`. Add corresponding
owner methods for late admission and terminal negative evidence. Do not expose a
generic `set_status` or a boolean `delivered` setter. Host code must retain the
exact attempt/row provenance when applying backend evidence.

A final local cancellation of a never-submitted row needs an explicit durable
disposition. Extend the checkpoint so the delivery traversal can pass a withdrawn
row without claiming the final consumer accepted it. Keep accepted-delivery
evidence distinct from the traversal position. Do not reinterpret the existing
acknowledgment watermark as proof that every skipped item was delivered.

Use the existing receipt checkpoint and envelope format versioning for these
extensions. Allocate the next incompatible checkpoint version, with strict
migration fixtures. Preserve original body/provenance on read. Legacy rows whose
past submission cannot be reconstructed become visibly unconfirmed; assigning
new native IDs and automatically replaying them is not a migration. Leave old
runs available for inspection and start the new delivery contract on a fresh run.
Older binaries must reject new-format inboxes rather than repairing away data.

An unconfirmed input at the head blocks later automated input until admission or
terminal disposition is established. It does not block status queries,
reconciliation, interruption, hosted results or actor retirement. Once admission
is durably known, later inputs may proceed while presentation is observed.
Notification uncertainty never acquires the separate typed request-settlement
fence. Backpressure and ordering must be visible to the host and operator.

Do not prune receipt context required to resolve a live request update. Extend the
existing bounded receipt policy with owner-held reconciliation retention, and
reject new tracked work when its bound is reached. Ordinary notification receipts
may expire under their existing bounded observation contract, but expiry must
remain evidence unavailable. Once the request owner has consumed a terminal
outcome, only bounded provenance and the native dedup watermark need remain.

## Request reconciliation

Extend `tidepool-actor/src/request/updates.rs`, not a second request registry.
Associate the original request/update with the durable inbox identity before the
transport attempt. A late reconciler can settle only that exact retained update
on the same actor incarnation.

- Presented evidence changes presenting/unconfirmed to presented and releases
  the presentation fence. It does not claim task incorporation.
- A native withdrawal or rejection that prevents later presentation records
  not-presented and releases that fence.
- Native admission alone retains an unconfirmed presentation outcome.
- Missing history, an expired record or loss of the native owner cannot release
  the fence as success or definitive non-presentation.
- Duplicate identical evidence is idempotent. Conflicting terminal evidence is
  a protocol failure retaining the original obligation, not last-writer-wins.

When uncertainty cannot be resolved, the operator can retire the actor's hosted
coordination under the recovery contract. Pending requests then terminate through
the existing actor-failure path, with unconfirmed delivery still reported. They
do not become successful replies or ordinary cancellation acknowledgments that
pretend the update was settled. The surviving TUI may remain usable manually.

Wire the existing notification send handoff to this same input path once native
admission and receipt observation are available. Preserve exact sender/target and
receipt authorization from the notification contract. Send returns its receipt
after durable host publication, not after incorporation; poll reports only the
evidence established by the transport. Capability failure before publication
continues to return unavailable without a fabricated receipt.

## Acceptance

- [ ] Lost queue admission reply, followed by another pending message, never changes or duplicates the first operation.
- [ ] Repeating a consumed key returns retained/compacted status and cannot start another turn.
- [ ] Repeating a key with different bytes or mode returns conflict.
- [ ] Crash after native dispatch claim never causes automatic redispatch.
- [ ] Crash after durable input append can reconcile presentation from canonical history.
- [ ] Delayed submit after exact-key withdrawal is rejected by its tombstone.
- [ ] A not-found query cannot clear an uncertain presentation fence.
- [ ] Update/reply and update/cancel races preserve the original request owner.
- [ ] Updates work while native execution is idle and the typed request remains open.
- [ ] Notification delivery leaves mounted request input/reply capability unchanged.
- [ ] Manual delete/edit of queued host input produces an honest correlated outcome.
- [ ] Native producer sealing prevents old queued input from executing in a resumed incarnation.
- [ ] Capacity pressure preserves unresolved evidence and does not deadlock status or stop.
- [ ] Host checkpoint, parent-directory and tail-repair failures retain existing strict durability behavior.
- [ ] Old inbox and native store migrations reject incompatible writers and never invent successful delivery.
- [ ] Bootstrap is delivered once after readiness with the intended original bytes.

Exercise host and native storage independently, then the real two-store consumer
under acknowledgment loss. A helper-only dedup test is not sufficient.
