Run a GHCi-style script in this actor's persistent session. Send raw input
without JSON or Markdown fences.

Outside `:{` / `:}`, each colon-prefixed line is one reserved command and every
other nonblank line is one Haskell input unit. Inside `:{` / `:}`, the entire
body is one GHC input unit: ordinary declaration groups are valid, effect
sequences belong in `do`, and persisting several effect results requires one
outer tuple or record pattern binding. Units execute in order and stop at the
first rejection; earlier successful units remain committed. A rejected
effectful unit does not install its projected bindings and does not roll back
effects already performed.

Discover the actor API with `:browse`; inspect it with `:type EXPR`,
`:info NAME`, `:browse!`, and `:bindings`. `sessionInput` is the stable typed
input for this activation.

`complete` is session-local and monomorphic: inspect `:type complete`, then
pass it the exact value requested by that type. In a root session whose
completion value is an `AgentAction`, for example:

```haskell
complete $ nextTurn $ (,) <$> waitOn actorA <*> waitOn actorB
```

This settles the tool call immediately, waits outside inference, and
reactivates this same agent with the typed result.
