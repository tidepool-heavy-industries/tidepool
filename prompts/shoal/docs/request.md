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
