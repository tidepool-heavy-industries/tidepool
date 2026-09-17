A record actor keeps coordination in Haskell so routine events do not wake a
model. One record describes private state, public calls, and fixed event
handlers; the same record supplies both the definition and the typed client.
`R` is `Tidepool.Actor.Record`.

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

`R.finish handle` drains accepted work and returns `ActorExit state`; retain
that value. It does not retire the workers whose results were observed — their
cleanup is a separate, explicit decision.

Route results, including their cleanup evidence, rather than waking a model to
poll. Actor-to-actor payloads are typed values or compact deltas, not narrated
snapshots; query only what the next engineering decision needs.

skill: shoal-define-actors
