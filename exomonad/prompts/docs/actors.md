Author a small control language and a stateful interpreter for it. A record
actor's calls and events are the vocabulary; its state carries the evolving
work; its handlers compose transitions, replies, agent RPC and Jev judgments.
The owner can query and steer the machine through its typed endpoints while
work proceeds. One record describes private state, public calls, and fixed event
handlers; the same record supplies both the definition and the typed client.
`R` is `Tidepool.Actor.Record`. `R.start` creates an explicit persistent service
with actor lifetime. It survives the creating invocation; ordinary command,
provider-worker and request defaults remain scoped to a hosted invocation.
Handlers without such an invocation use actor ownership for their work.

Use a command sum through one `Call` for a single interpreter, or separate typed
calls for distinct operations. Project event sources into a shared sum with
`fmap` and merge them with `<>`. The same structure can coordinate candidates and
reviews, hypotheses and experiments, or any task with evolving state and
meaningful arrivals. Workspace actors and collectors are parts you can compose;
`doc topics` lists compiled modules and `lookup` browses their declarations.

This collector uses one `State`, one query `Call`, and one `Event` over the
settlements it collects:

```haskell
data Results mode = Results
  { resultState :: mode :- State [Either ResponseFailure (ResponseResult (Outcome Candidate))]
  , arrived :: mode :- Event (Either ResponseFailure (ResponseResult (Outcome Candidate)))
  , resultCount :: mode :- Call () (R.Reply Int)
  }
let resultDefinition = coordinationActor "candidate-results" Results
      { resultState = []
      , arrived = R.on (R.settlement worker) (\result -> modify' (++ [result]))
      , resultCount = \() -> gets length
      }
results <- R.start resultDefinition
count <- R.call (resultCount (R.client results)) ()
display count
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

Route results, including their cleanup evidence, into the next transition.
Choose a useful payload: a compact delta, a structured report, a narrative
explanation or another Haskell value. Query the state the next decision needs.

`coordinationActor` is the shared `Exomonad.Contrib.Actors` wrapper around
`R.definition name (Actor.Selected knownEffects)`. `Outcome` and `Candidate`
come from `Exomonad.Contrib.Types`. Import configured contrib modules when they
are not already in scope; project-specific task and model policy stays in `Project.Work`.

A direct `R.definition` needs a pinned row, for example
`:: ActorSpec MyActor MyEffects`: `knownEffects` is polymorphic and otherwise
ambiguous. `Jev` and `Commands` are effect types from
`Tidepool.Effects.Core` and need an import before a row can name them.

For Git delivery, load `exomonad-project-work` for the workflow. Its supplied
compositions include `unfoldWork` batch collection and `startReviewFlow` review
and repair. Use them as parts of your interpreter, or connect the primitives
directly. `exomonad-coordinate` and `exomonad-review` explain those operations.

skill: exomonad-define-actors
