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

Request activations show a rendered `sessionInput` preview and the reply type's
GHC declaration captured at its typed request site when available. Input previews
use the workbench's compact display (a 512-character payload prefix), with an
outer 16 KiB message cap; reply definitions are capped at 4 KiB. Omitted detail
is marked explicitly. The mounted value remains
authoritative. Opaque inputs remain valid; inspect their types and project useful
fields. Reply-type dependencies are not expanded automatically. No second type lookup is performed at activation. Older artifacts without a
captured declaration still show the exact reply type. Previewing does not settle the request.

Preview limits bound message size, not the cost of a custom `Show` implementation.
Rendering uses the shared resident execution mechanism with a compiler-checked
pure expression; there is no separate preview timeout. A missing rendering
instance produces an opaque-value marker. Previewing an effect-valued input
does not execute the action it contains.
