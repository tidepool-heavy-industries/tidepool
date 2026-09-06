`request` submits one typed assignment to a retained actor. The `Response a`
handle is durable while its resident Haskell machine lives; readiness alone
does not spend another model turn.

```haskell
let Right label = requestLabel "review-change"
response <- request @Report worker label task
pollResponse response
```

Here `worker` is an existing `AgentRef`, `task` is your typed input, and `Report`
is your declared result type. The explicit result type keeps submission
unambiguous before any consumer is defined. Use `requestWith` to add guidance
or a dimensional deadline to the labeled request.
Replying settles this request, not the actor, so the same `AgentRef` can accept
later refinements.

An actor serves one request at a time. A follow-up to a busy actor queues another
assignment; do not assume it steers the active request. Establish coordination
before forking: identify the contract revision, owned deliverable, acceptance
condition, and decisions requiring a checkpoint. A worker needing a decision
should state the choice and what can continue. Avoid circular request waits.
Send revised contracts with their commit and decision delta; confirm which
revision a returned candidate satisfies. Do not treat cancellation as an
acknowledged contract update or a successful pause of the whole subtree.

To clarify the current assignment, target its existing `Response`:

```haskell
Right clarification <- updateRequest response "Tabs must also respond to mouse clicks."
pollRequestUpdate clarification
```

This wakes the same conversation if idle or presents input after the current
model/tool boundary. It preserves `sessionInput`, `sessionReply`, and the original
response. Existing tool effects stay committed. A second `request` still means
a separate queued assignment.

`pollRequestUpdate` returns `Right` with one of these observations:

- `UpdateQueued`: awaiting confirmed presentation, including delivery in flight.
- `UpdatePresented`: inserted into conversation history; this does not establish
  understanding, incorporation, checks, or acceptance.
- `UpdateTooLate`: the original request ended or was cancelled/abandoned before
  delivery could start; no new assignment was created.
- `UpdateNotPresented reason`: delivery failed before input was submitted.
- `UpdateUnconfirmed reason`: input may have been submitted, but presentation
  could not be proved. It is not automatically resent.

Only the exact request owner can send or inspect updates. One unpresented update
may be outstanding per request; another returns `Left ReplyUpdatePending`.
An assignment that is still queued returns `Left ReplyStale`; send its eventual
clarification once it is active. Update observations live with the response's
metadata; forgetting the response makes its update handles stale.

During delivery, `attemptReply` and `attemptAcknowledgeCancellation` return
`Left ReplyUpdatePending`. After an unconfirmed delivery they remain fenced so
late input cannot affect the next assignment. You can request cancellation or
stop the actor; stopping it is the recovery path for unresolved delivery.
A proven failure before submission releases the fence. The current backend
waits up to five minutes for confirmation after connection, so a longer tool
call can leave an update unconfirmed. Report intended changes and later
incorporation with task-specific progress or a typed result.

Request activations present `Text` inputs as assignment prose, up to 16 KiB of
UTF-8 text, without Haskell string quoting. Read it there; `sessionInput` retains
the exact input for later use. Structured inputs use the ordinary compact display
(a 512-character payload prefix by default). Both have a 16 KiB byte cap, followed
by an omission cue where needed. Expand omitted prose directly with
`inspectFull sessionInput`; a bare `sessionInput` observation still uses the
ordinary quiet display. Explicit text inspection returns the original text
without a `Show` conversion, quoting, or escaping. Opaque inputs remain valid:
use their types to select fields or apply them. Other values use `Show` by default.

The reply type's GHC declaration is captured at its typed request site when
available and capped at 4 KiB, with a direct `:info` cue if truncated. Reply-type
dependencies are not expanded automatically; no second type lookup is performed
at activation. Older artifacts without a captured declaration still show the
exact reply type. Presentation does not settle the request.

Preview limits bound message size, not the cost of a custom `Show` implementation.
Rendering uses the shared resident execution mechanism with a compiler-checked
pure expression; there is no separate preview timeout. A missing rendering
instance produces an opaque-value marker. Previewing an effect-valued input
does not execute the action it contains.
