`request` submits one typed assignment to a retained actor. The `Response a`
handle is durable while its resident Haskell machine lives; readiness alone
does not spend another model turn.

```haskell
response <- request worker task
pollResponse response
```

Use `requestWith` for a readable label, guidance, or a dimensional deadline.
Replying settles this request, not the actor, so the same `AgentRef` can accept
later refinements.
