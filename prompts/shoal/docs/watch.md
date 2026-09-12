Requests wake their owner at terminal settlement by default. A labeled
`Watch a` retains a finite applicative join; set `report = Silent` on requests
whose settlement is already owned by that watch or by a record actor.

```haskell
let joinLabel = "first-wave-results" :: WatchLabel
:{
joined <- watch joinLabel $
  (,) <$> awaitSettled (fst workers) <*> awaitSettled (snd workers)
:}
```

A combined watch is appropriate when the next decision needs both results.
Independently integrable work can use the default settlement notices. A wave
router remains useful when project code must retain progress or apply custom policy.

End the model response normally. When the watch becomes terminal, Tidepool
reactivates the actor. `pollWatch joined` returns its retained typed observation.

For ongoing progress, use a persistent Haskell actor with `progressSource`.
It captures current state and then receives every publication and closure in
order. The curated `Project.Routing.followWork` collects named progress/response pairs, retains
evidence, per-source questions and terminal receipts, and invokes an authored sink
on meaningful changes. The sink chooses meaningful messages; no model rearms watches or relays
routine progress. Source completion does not terminate the collector.

`Await a` is the pure dependency description; `Watch a` is its registered
finite observation. `ReplyAvailable` carries a typed result and its evidence;
`ReplyUnavailable` carries a typed failure. Use `awaitResponse` instead when every
dependency must succeed. Polling a settled watch repeatedly returns its state
without consuming it. Compose dependencies before registration.

`pollWatch joined` displays a compact lifecycle summary and saves the observation.
Its output names the exact `inspectFull (...)` expression for the saved result.
To keep evidence beyond the latest eight automatic observations, bind it:

```haskell
joinedState <- pollWatch joined
inspectFull joinedState
```

Expansion does not poll or repeat effects. For a smaller task-specific view of
the two `Text` reports above, this projection preserves pending and failure states:

```haskell
:{
reportPairView :: WatchState (Settlement Text, Settlement Text)
               -> WatchState (Either ResponseFailure Text, Either ResponseFailure Text)
reportPairView = fmap (\(left, right) -> (settledValue left, settledValue right))
:}
joinedState <- pollWatch joined
inspectFull (reportPairView joinedState)
```

`fmap` transforms only a ready value; pending and unavailable states survive.
For a watch built with `awaitResponse`, use `fmap responseValue` instead:
its ready payload is `ResponseResult a`, not `Settlement a`.

Inspect `joinedState` further when deciding integration or checking provenance.
`responseValue` is the actor's reported conclusion; `responseExecution` and
`responseWorktree` are runtime evidence. A successful reply does not establish
review, acceptance, or integration: those remain the responsible coordinator's
judgments about an exact candidate. Project task-specific report fields when
even the report is large; no standard worker ledger is required.

A wake notification is a reason to inspect, not a replacement for the handle's
current state. On a delayed or duplicate notice, poll the watch before acting;
do not resubmit the original work merely because another notice arrived.

`requestWithProgress @Progress @Result actor options` returns a response and
a `Progress Progress` handle. `childWithProgress @Progress @Result branch`
returns the corresponding `(Response Result, Progress Progress)` inside an
unfold. The target receives `reportProgress :: Progress -> Eff effects ()`.
Payloads can contain session-defined ADTs and closures; no `Show` or encoding
instance is required.

For a retained `lead :: AgentRef`, this requests cumulative nonterminal
findings (`[Text]`) and a final `Text` reply:

```haskell
let progressOptions = assignment "lead-findings" ("Publish cumulative findings; then return your final report." :: Text)
(leadResponse, leadProgress) <- requestWithProgress @[Text] @Text lead progressOptions
let findingsLabel = "lead-findings-ready" :: WatchLabel
findingsReady <- watch findingsLabel (awaitProgressAfter leadProgress (ProgressCursor 0))
let reportLabel = "lead-report-ready" :: WatchLabel
reportReady <- watch reportLabel (awaitSettled leadResponse)
```

Choose progress at assignment creation when a lead's intermediate findings matter.
It does not change or amend an already active assignment. The lead can publish
`reportProgress ["finding"]` while keeping its final reply pending. End the turn
when waiting. This finite watch captures the first finding; ongoing following uses
a persistent source actor. Inspect the separate final-report watch on its wake.
Progress is one-way observation, not a return steering channel: it does not repair
failed amendments or provide a root handle. A queued follow-up still cannot
unblock a lead waiting synchronously for it; avoid circular waits.

`pollProgress updates` observes the latest update. Register
`watch label (awaitProgressAfter updates (ProgressCursor 0))` to wait for the
first update or closure. These finite observations have independent cursors and
can coalesce updates. For lossless ongoing delivery, attach `progressSource`
to a persistent actor instead of building a rearming loop. A watch retains its qualifying snapshot, so
later publications cannot change the value obtained by polling that watch.
Mixed response/progress watches retain each qualifying progress snapshot while
waiting for their remaining dependencies. `ProgressClosed` ends a wait with
no qualifying update. Already captured snapshots remain valid after closure.
Unwatched progress never wakes the coordinator.

For intermediate Git publications, make the progress payload cumulative. With
`Increment` defined for this task and `interfaceIncrement` / `implementationIncrement`
containing exact commits, purposes, and consequential discoveries, a worker
launched with progress type `[Increment]` can publish in separate tool calls:

```haskell
reportProgress [interfaceIncrement]
```

Later, while retaining its original delivery request:

```haskell
reportProgress [interfaceIncrement, implementationIncrement]
```

A coordinator that misses the first update still sees both publications. Keep
unacknowledged increments in later snapshots and the final delivery. Publication
is not parent acceptance. The coordinator inspects and integrates each selected
commit, then communicates the accepted baseline and decision delta. A normal
`request` to a busy specialist queues; this example does not provide mid-flight
steering or a synchronous checkpoint. Never wait circularly for that request.
