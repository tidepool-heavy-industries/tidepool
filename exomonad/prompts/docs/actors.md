A record actor keeps coordination in Haskell so routine events do not wake a
model. One record describes private state, public calls, and fixed event
handlers; the same record supplies both the definition and the typed client.
`R` is `Tidepool.Actor.Record`.

Find out whether this workspace already authors one that fits. `doc topics` ends
by naming its compiled modules and `lookup` on one of those names browses their
declarations, including the outcomes they can return. Some workspaces ship types
and helpers and no actor at all; authoring your own is then the right move. What
costs turns is rebuilding in shell what an installed actor already does.

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
publications accepted while a handler is busy stay ordered. Handlers remain
serialized while suspended; never await an event requiring another handler on
that same mailbox to run.

`R.withWorktree tree spec` starts the actor holding a worktree the parent
created and did not bind. Worktree ownership is exclusive and integrate
authority follows the owned handle, so the actor that merges into a worktree is the actor that holds it;
an actor with a worktree resolves to the coding role, one without to research.
At most one worktree per actor.

`R.finish handle` drains accepted work and returns `ActorExit state`; retain
that value. It does not retire the workers whose results were observed — their
cleanup is a separate, explicit decision.

Route results, including their cleanup evidence, rather than waking a model to
poll. Actor-to-actor payloads are typed values or compact deltas, not narrated
snapshots; query only what the next engineering decision needs.

`coordinationActor` is the shared `Exomonad.Contrib.Actors` wrapper around
`R.definition name (Actor.Selected knownEffects)`. `Outcome` and `Candidate`
come from `Exomonad.Contrib.Types`. Import configured contrib modules when they
are not already in scope; project-specific task and model policy stays in `Project.Work`.

A direct `R.definition` needs a pinned row, for example
`:: ActorSpec MyActor MyEffects`: `knownEffects` is polymorphic and otherwise
ambiguous. `Jev` and `Commands` are effect types from
`Tidepool.Effects.Core` and need an import before a row can name them.

For execution coordination, use the installed recursive-work procedure:
`unfoldWork` admits a ready batch and retains its event collector; each owner
integrates checked children and repeats locally. Load `exomonad-coordinate` for
that procedure and `exomonad-review` for counted checks, exact-source review and
bounded repair through `startReviewFlow`. Author a custom record only for a
specific join or routing decision those compositions do not express.

skill: exomonad-define-actors
