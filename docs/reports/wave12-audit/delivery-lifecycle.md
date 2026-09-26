# Wave 12 notification and request-update delivery trace

Run `7d0bd907-1640-49b1-a952-f98005ca6e2d`. This trace uses the
host JSONL, exact actor inbox rows, actor lifecycle bindings, and native rollout
user-message timestamps. It extends the C06/C07 audit beyond its 02:53:28 UTC
cutoff to 03:02:10 UTC. No live process was restarted or steered. Raw message
content stays in the private artifacts named by `coordination.md`.

## The two delivery paths

| Stage | Tracked notification or update | Settlement/watch notice |
|---|---|---|
| Enqueue | `actor_host.rs` publishes a tracked inbox row with exact actor and request/update provenance. | `publish_request_notifications` publishes an untracked typed inbox row. |
| Admission | The one-second delivery pump takes only the front tracked row. `input_control::submit` records its producer and inbox sequence in native queued-input storage. An `Admitted` response means durable native queue admission. | A notice behind a submitted/unconfirmed tracked row may overtake it. `deliver_out_of_order_notices` calls `backend.push`, which runs `codex queue --message`. Its success means the CLI queued the text, with no per-notice presentation receipt. |
| Presentation | Native queue dispatch confirms `Presented` only after `start_or_steer_turn` starts or steers with that exact input. The host queries the same operation, confirms the inbox row, and reconciles an update. | The host marks the notice “surfaced” in memory immediately after the queue CLI succeeds. Actual model input is visible later as a native rollout user message; the host has no corresponding confirmation. |
| Consumption | The recipient can act on the next native turn. A request reply is fenced until the update is presented or proven not presented. | The owner can independently read a settled `Response` through `pollResponse` before the queued notice appears. That read does not record a settlement-notice acknowledgement. |

The host's former “delivered settlement/watch notice queued behind a stuck native
delivery” log named a successful *queue call*, not model presentation. It now
says the notice was queued. The
tracked path keeps the distinction in its receipt state. The untracked path
does not. Relevant owners: `bridge/facade/src/actor_host.rs`
(`deliver_pending_checked`, `deliver_out_of_order_notices`,
`deliver_tracked_message`), `exomonad/node/src/inbox.rs`
(`legacy_notices_beyond_barrier`, `mark_surfaced_out_of_order`),
`exomonad/agent/src/backend/codex/node.rs` (`queue_message`), and the
pinned native queue service in
`vendor/codex/codex-rs/ext/queue/src/service.rs`.

## C06: update to actor 2, request 14

The update admitted at 02:44:33 became actor 2 inbox row 8. Row 8's exact
payload matches the native user message at **03:00:33.168 UTC**. The message
at 02:50:52.239 matches earlier tracked row 5; the message at 02:58:02.309
matches earlier row 7. The 02:51:14.557 user message does not contain row 8's
update payload. Therefore the `inspectFull sessionInput` call at 02:51:17
is not presentation evidence for this update. The C06 audit correctly left
incorporation unverified at its cutoff, but its explanation of that call as
the update's next turn is contradicted by the exact inbox-to-rollout match.

The host marked update presentation `Unconfirmed` at 02:44:40.625
*immediately after durable enqueue*: this is the intended state for an
asynchronous tracked row, not a seven-second delivery timeout. Row 5 was
queried as native `Admitted` through 02:50:34 and recovered at 02:50:52.
Row 7 then remained `Admitted` through the audit cutoff and recovered at
02:58:02. Row 8 remained `Admitted` from 02:58:03 until presentation and
host recovery at 03:00:33.874. The two candidate replies at 02:50:41 and
02:51:05 were refused by the update fence while row 8 was still waiting.
The 02:50:32 parser error is independent of this fence.

This sequence shows ordered backlog plus active recipient turns. It does not
show a lost update wake or a backend rejection. The pinned native service
persists each input, calls `wake_if_loaded` on admission, and dispatches
queued input at the thread idle boundary; the observed native turns and host
recoveries show that boundary eventually advanced. The exact content the
recipient incorporated after 03:00:33 remains a separate task-level check.

## C07: root settlement notices

The host pushed 17 settlement notices past tracked rows between 02:11:08 and
02:48:07. Matching each notice's request ID and label (or command job ID) to
the root native rollout finds **17 of 17** later user-message presentations.
The delay from queue CLI success to native user message ranged from **94 to
1,252 seconds** (median **479 seconds**). Selected anchors:

| Inbox row | Host queue success UTC | Native user message UTC | Delay |
|---:|---|---|---:|
| 6 | 02:11:08 | 02:19:57 | 528 s |
| 8 | 02:11:34 | 02:31:44 | 1,210 s |
| 12 | 02:17:43 | 02:38:34 | 1,252 s |
| 22 (command job) | 02:39:37 | 02:43:24 | 227 s |
| 24 | 02:42:12 | 02:43:46 | 94 s |
| 29 | 02:48:07 | 02:52:07 | 240 s |

Root had already obtained Ready or Unavailable results in sampled
`pollResponse` calls. The late native messages thus caused some turns to
revisit old request IDs after candidates had been repaired or merged, as the
product friction report records. A queue receipt alone could not tell the
host that the model saw a notice, and a response read did not invalidate a
native notice already queued. The evidence shows delayed presentation, not
lost notice delivery. It does not establish how many model calls would be
saved by suppressing stale messages.

## Failure, cancellation, and restart boundaries

- A tracked row keeps its exact native operation ID. Failed submission before
  acceptance can be retried; uncertain/submitted outcomes are queried and
  never blindly resubmitted. Native withdrawal after absent evidence can
  requeue a notification with a “possibly already seen” marker. A request
  update is never silently requeued as a replacement; its original
  `Unconfirmed` or `NotPresented` evidence remains attached to the request.
- The inbox only advances its cursor after exact presentation or terminal
  native evidence. Later tracked rows cannot pass the front row. Legacy
  settlement/watch notices may overtake an in-flight row, but the set marking
  them as already queued out of order is **in memory only**. A host restart
  before the barrier clears can queue a second copy. The code explicitly
  accepts that duplicate; the sampled run does not demonstrate a restart.
- The old-notice problem cannot be solved by dropping all settled notices:
  `pollResponse` may not have happened, and a notice may be the owner's only
  wake. The registry records when a *watch* was observed settled, but
  `observe_response` records no equivalent response-consumption time.
  The ordinary inbox path already suppressed forgotten watches and transitions
  observed through `pollWatch`; the out-of-order path skipped those checks.
  Both paths now use one selection function. Suppressed rows behind a barrier
  remain behind the cursor and are marked surfaced in memory so they stay
  silent when the barrier clears.
- `codex queue` prints a native queued submission ID, but Tidepool discards
  that output. Native app-server exposes queue list, delete, update, and
  reorder, but no presentation receipt for ordinary queue items. A failed
  delete is ambiguous: an item might already have been dispatched. The CLI
  generates a fresh client message ID for every enqueue; it is not an
  idempotency key in queue storage. Capturing the printed ID alone leaves a
  duplicate window after native acceptance and before host recording.

## Structural decision

The observed root latency follows two queues: the host inbox barrier and
the native user-input queue. Native `Admitted` is a durable input, while
model-visible presentation waits for a turn boundary. This change gives
untracked notices an accurately named “queued” log and shares authoritative
watch suppression across both host delivery paths. It does not change native
dispatch. A full correction needs a native receipt or an explicitly weaker
“queued” contract for every notice, plus a way to record exact response
observation and supersede a queued notice before presentation. The owned
candidates are:

1. Extend the existing input-control producer with durable, cancellable
   notice operations on a separate priority lane. Keep per-notice identity
   through admission, query, presentation, withdrawal, restart, and inbox
   acknowledgement. Suppress only a notice proven observed or superseded;
   retain meaningful unconsumed outcomes. This provides exact proof but
   changes the pinned native queue and host transport together.
2. Keep `codex queue` as a best-effort notification enqueue. Capture its
   native queue item ID, expose native deletion, and record response observation
   to withdraw stale items before dispatch. This can reduce some duplicate
   turns but cannot prove presentation or be crash-safe without a native
   idempotency/query contract.

The first is the complete contract. The second is a bounded improvement with
the stated limit, not a fix for the 17 observed post-enqueue delays.

## Bounded transport experiment

Use the existing opt-in rollout trace and host JSONL on one controlled actor
conversation. Queue one tracked update and one settlement/watch notice while
a provider turn is active, then record durable host enqueue, native admission,
native `Presented` for the tracked row, native user-message insertion for the
ordinary notice, and the next model turn. Repeat with a short idle turn and a
held hosted Haskell cell. Compare accepted-to-presented distributions and
native queue depth; no live wave-12 daemon needs to be restarted. Include a
case where `pollWatch` observes a settled transition before the out-of-order
notice is selected, and a newer transition that must still surface.

Prototype active-turn dispatch only behind a separate test switch. Before any
native steer, recheck the hosted-call settlement gate **at dispatch time**:
the existing admission check can become stale when a cell starts after the
input was queued. Test simultaneous cell start, cancellation, withdrawal,
restart, and repeated producer sequence. Do not promote active steer unless
the cell remains uninterruptible, one update is presented exactly once, and
the reply fence releases only on exact presentation or proven nonpresentation.
The 17 observed notice delays establish a latency problem; they do not
establish a safe priority policy or a measured speedup for this experiment.
