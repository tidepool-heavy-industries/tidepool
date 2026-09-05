Polling is authoritative and never wakes an idle model by itself. A labeled
`Watch a` is the explicit durable subscription for one applicative fold.

```haskell
let Right joinLabel = watchLabel "first-wave-results"
:{
joined <- watch joinLabel $
  (,) <$> awaitSettledFork (fst workers) <*> awaitSettledFork (snd workers)
:}
```

End the model response normally. When the watch becomes terminal, Tidepool
reactivates the actor and `pollWatch joined` returns its typed observation.

`Await a` is the pure dependency description; `Watch a` is its registered
subscription. `ReplyAvailable` carries a typed result and its evidence;
`ReplyUnavailable` carries a typed failure. Use `awaitFork` instead when every
dependency must succeed. Polling a settled watch repeatedly returns its state
without consuming it. Compose dependencies before registration.

A wake notification is a reason to inspect, not a replacement for the handle's
current state. On a delayed or duplicate notice, poll the watch before acting;
do not resubmit the original work merely because another notice arrived.
