`result request` is the typed readiness description for a request. Compose it with
other `Await` values and observe the composition through the single `await`
operation. A request's watch owner retains its decision; callers do not repoll
branches to reconstruct it.

For two independent typed requests, applicative composition observes both and
returns their typed results together:

```haskell
Right reports <- await ((,) <$> result firstRequest <*> result secondRequest)
display reports
```

Applicative waiting requires each branch to succeed. A first-terminal choice
includes failure and is left-biased on an initial tie. A settled branch within an
unfinished applicative parent may latch a choice; a losing branch cannot poison
its winner. Settlement projections expose branch failures as values when an
authored collector needs all outcomes. Traversal works over collections of
requests without erasing their result types.

Use `requestWithProgress` when intermediate updates are independently useful.
Its progress handle has its own typed lifecycle and does not replace or amend the
final request. Registration or waiting does not transfer ownership. A waiting
cancellation does not cancel the request; request cancellation does not retire
the actor. Default request ownership belongs to the caller. Explicit scopes are
joined only by resources whose options use `InScope scope`.

The watch owner retains immutable readiness decisions and selected response or
progress leases. Releasing a losing response after the decision does not revoke
the terminal snapshot. If a handle is already unavailable when observed, retain
that typed failure. A successful reply remains the agent's authored conclusion;
review, acceptance, and integration require their own evidence.

Use record actors for ongoing stateful routing across events. Keep their event
handlers able to run: a handler must not wait for an event that requires another
handler on its own serialized mailbox. Load `exomonad-agent-work` for typed
requests and `exomonad-define-actors` for persistent event routing.
