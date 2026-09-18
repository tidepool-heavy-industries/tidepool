A record actor keeps coordination in Haskell so routine events do not wake a
model. One record describes private state, public calls, and fixed event
handlers; the same record supplies both the definition and the typed client.
`R` is `Tidepool.Actor.Record`.

Check whether this workspace already authors the one you want. `doc topics`
ends by naming the workspace's compiled modules and `lookup` on one of those
names browses its declarations, including the outcomes it can return. Starting
an existing actor is `R.start` plus a call per candidate; rebuilding what it
does out of shell commands costs turns and tests nothing.

The minimal shape is a record with one `State`, one `Call`, and one `Event` over
the settlements you want collected:

```haskell
import GHC.Generics (Generic)
data Results mode = Results
  { resultState :: mode :- State [Either ResponseFailure (ResponseResult (Outcome Candidate))]
  , arrived :: mode :- Event (Either ResponseFailure (ResponseResult (Outcome Candidate)))
  , resultCount :: mode :- Call () (R.Reply Int)
  } deriving Generic
let resultDefinition = coordinationActor "candidate-results" Results
      { resultState = []
      , arrived = R.on (R.settlement worker) (\result -> modify' (++ [result]))
      , resultCount = \() -> gets length
      }
results <- R.start resultDefinition
R.call (resultCount (R.client results)) ()
```

`State s` appears exactly once; its definition field is the initial value, and
handlers use ordinary `get`, `gets`, `put`, `modify'`. A `Call input NoReply` is
sent with `R.send`; a `Call input (R.Reply output)` is called with `R.call`.
`R.client handle` is the typed client: state and event fields are private in it,
so passing one endpoint grants exactly that route.

`R.settlement response` publishes the exact typed terminal result, including
failure and execution evidence; `R.progress p` publishes progress; and
`Cmd.completion job` publishes one retained command result. Attachment starts
from retained current source state, not a replay of earlier history, and
publications accepted while a handler is busy stay ordered.

`R.withWorktree tree spec` starts the actor holding a worktree the parent
created and did not bind. Custody is exclusive and integrate authority follows
custody, so the actor that merges into a worktree is the actor that holds it;
an actor with a worktree resolves to the coding role, one without to research.
At most one worktree per actor.

`R.finish handle` drains accepted work and returns `ActorExit state`; retain
that value. It does not retire the workers whose results were observed — their
cleanup is a separate, explicit decision.

Route results, including their cleanup evidence, rather than waking a model to
poll. Actor-to-actor payloads are typed values or compact deltas, not narrated
snapshots; query only what the next engineering decision needs.

`coordinationActor` above is not shipped: it is a one-line wrapper that
`examples/shoal-workspace` wrote in its own `.shoal/Project/Actors.hs`, as
`R.definition name (Actor.Selected knownEffects)` over
`LocalEffects api '[Replies, Actor, Notifications]`. `Outcome` and `Candidate`
are that workspace's types too. In a fresh project, write the same wrapper into
your own `.shoal/Project`, or call `R.definition` directly and pin the row with
`:: ActorSpec MyActor MyEffects` — `knownEffects` is polymorphic in the row and
ambiguous without it. `Jev` and `Commands` are effect types from
`Tidepool.Effects.Core` and need an import before a row can name them.

When the record is carrying a whole implement → review → repair → merge loop,
load `shoal-orchestrate`: the record, the seven conditions worth waking the
owner for, and the decisions kept in its state that the owner reads with one call.

skill: shoal-define-actors
