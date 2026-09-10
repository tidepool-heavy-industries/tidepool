# The concrete actor loop

The [coordination RSI plan](coordination-rsi.md) tracks implementation and acceptance.
The executable usage lives in the curated workspace rather than parallel sketches:

- [Defining actors skill](../../examples/shoal-workspace/.shoal/skills/shoal-define-actors/SKILL.md): one generic record, state handlers, typed clients, fixed event sources and explicit finish.
- [Project actor defaults](../../examples/shoal-workspace/.shoal/Project/Actors.hs): selected effect row and scoped worker release.
- [Local collection](../../examples/shoal-workspace/.shoal/Project/Routing.hs): ordered deltas, original typed results, compact current views and exact incorporated evidence.
- [Typed component handoff](../../examples/shoal-workspace/.shoal/checks/handoff-router.hs): independent final results cross a typed parent endpoint. The owner still integrates and checks actual source.
- [Known review continuation](../../examples/shoal-workspace/.shoal/checks/review-continuation.hs): an available retained reviewer receives the selected candidate; results return directly to the owning actor.
- [Continuation usage](../../examples/shoal-workspace/.shoal/plans/continuation.md): required bindings, request retention and completion.

The defining shape is a record parameterized by its interpretation:

```haskell
data Join mode = Join
  { state :: mode :- State JoinState
  , apiDone :: mode :- Event ApiResult
  , runtimeDone :: mode :- Event RuntimeResult
  , incorporated :: mode :- Call IntegrationEvidence NoReply
  , brief :: mode :- Call () (R.Reply IntegrationBrief)
  } deriving Generic
```

`JoinState`, `ApiResult`, `RuntimeResult`, `IntegrationEvidence` and
`IntegrationBrief` are project-defined types in this illustrative schema, not
built-in stages. The definition supplies the initial state, handlers and
`R.on source handler` bindings. Generic derivation supplies endpoint values.
`State` and `Event` expose no remote client capability. Each actor has one
sequential mailbox; handlers use ordinary state effects. `R.self @Join` supplies
send-only return endpoints for later continuations.

The actor follows the actual dependency join, not an artificial admission batch.
Keep ordinary applicative unfolding and explicit model/context choices. Bind
request/progress handles directly; a bundle is useful only where it removes real
repetition. Do not make every worker spawn create another integration stage.

For ordinary observation, use `followWork` once. For a known continuation, the
actor submits directly to an available worker and installs the result route.
`requestWithProgressInto` first gives a retention callback the exact handles, then
admits the request. The review example transfers those handles to an owned mailbox
before admission, independent of the submitting handler's final state checkpoint.
External effects are not rolled back; replacement must never replay uncertainty.

Events carry changed facts and references to retained evidence. Full distinct
publications remain in local history; only outstanding engineering appears in the
normal brief. An explicit incorporated message removes exactly the handled
candidate from that view, not every artifact sharing its commit. The parent gets
an actionable typed result or compact decision packet, not recursive history.

A final review result drains that attempt's collector automatically. The integration
actor remains available through repairs; the owner explicitly finishes it and
retains its exit. Worker release is separate and uses the existing cleanup owner's
admission and custody fences. No model turn exists solely to rearm, relay a known
result, narrate queue state or close a finite observer.

The acceptance checks must execute these files, including admission followed by
handler failure and replacement, meaningful source checks, repeated review and
integration. Compiling the record schema alone is not next-run acceptance.
