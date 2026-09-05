Polling is authoritative and never wakes an idle model by itself. A labeled
`Watch a` is the explicit durable subscription for one applicative fold.

```haskell
let Right joinLabel = watchLabel "first-wave-results"
:{
joined <- watch joinLabel $
  (,) <$> awaitFork (fst workers) <*> awaitFork (snd workers)
:}
```

End the model response normally. When the watch becomes terminal, Tidepool
reactivates the actor and `pollWatch joined` returns its typed observation.
