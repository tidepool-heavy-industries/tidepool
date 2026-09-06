# Inbox-backed notification evidence

Extend the sole DurableInbox<T, R = ()>, with generic host-owned provenance R
on tracked original rows and bounded small receipt evidence in its existing
checkpoint. Node never depends on actor types. Actor host uses provenance with
exact sender/target; receipt contains existing inbox namespace + sequence.

Required API (worker may refine lifetimes, not semantics): publish_tracked(payload,
context), observe_receipt(sequence), begin_tracked_delivery(sequence), and a
noncloneable attempt token with typed not_submitted/submitted/unconfirmed terminal
methods. confirm_presented(sequence) requires caller-held correlation evidence;
no old acknowledge or cursor inference may call it. Observation retains context
and phase; expired/unknown/untracked is unavailable. Phases: Accepted, InFlight,
Submitted, Presented, Unconfirmed. Reopen turns InFlight into Unconfirmed.

Before sendable token escapes, persist pre-send fence. IO uncertainty poisons
further mutation until reopen; never reuse a possibly appended sequence. Ordinary
ack cannot leap an unaccepted tracked row. Submitted is native consumer acceptance,
not model presentation. Acceptance/checkpoint cursor advancement must be one atomic
checkpoint update; correlated presentation may arrive later. Mixed delivery runs
legacy prefix only before first tracked row, never batching tracked notifications
into assignments. Unconfirmed cannot be automatically retried.

Retention: bounded count and serialized provenance size; evict only acknowledged
receipts, never unresolved pre-send fences. Refuse new tracked admission on capacity
rather than lose safety. Preserve sender attribution after row ack/compaction by
small R evidence in the existing checkpoint; no copied message bodies or extra log.

Explicit migration decision: ordinary legacy use remains supported. Before first
tracked publication, durably upgrade the EXISTING checkpoint to a versioned nested
shape using the existing version_ladder owner. No legacy top-level sequence field:
old binaries must reject, rather than ignore receipt fields and silently retry.
Legacy numeric/object cursors migrate without fabricating receipt evidence. Future
versions and inconsistent row/provenance/sequence fail closed. New-format inboxes
must not be opened by old hosts. This does not migrate any running host in tests.

Inbox worker exclusively owns tidepool-node/src/inbox.rs and exports/tests. TL owns
actor host/provenance adapter and mixed queue tests. Source/host consumer migration
must be checked together before product acceptance. Native controller remains
unavailable, and production notification admission rejects before publication until
service supplies supported delivery; in-process fixtures can validate real inbox
admission/observation without claiming native presentation.
