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

`requestWithProgress @Progress @Result actor options` returns a response and
a `Progress Progress` handle. `childWithProgress @Progress @Result branch`
returns the corresponding `(Forked Result, Progress Progress)` inside an
unfold. The target receives `reportProgress :: Progress -> Eff effects ()`.
Payloads can contain session-defined ADTs and closures; no `Show` or encoding
instance is required.

`pollProgress updates` observes the latest update. Register
`watch label (awaitProgressAfter updates (ProgressCursor 0))` to wait for the
first update or closure, and rearm with the revision from `ProgressUpdate`.
Each observer has its own cursor. Updates can coalesce; this is latest-value
progress, not a message queue. A watch retains its qualifying snapshot, so
later publications cannot change the value obtained by polling that watch.
Mixed response/progress watches retain each qualifying progress snapshot while
waiting for their remaining dependencies. `ProgressClosed` ends a wait with
no qualifying update. Already captured snapshots remain valid after closure.
Unwatched progress never wakes the coordinator.
