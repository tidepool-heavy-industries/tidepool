# The concrete actor loop

These are target sketches for [the RSI pass](coordination-rsi.md), not expressions
available in the current package. Implementation must turn this exact flow into
an executable skill/check, with real imports and inferred types. The new pieces
are effectful actor definitions, a typed self mailbox and owned forwarding.

## What the owner writes

Keep ordinary applicative unfolding and explicit model/context choices:

```haskell
(api, runtime) <- unfold group $
  (,) <$> Work.child apiBranch <*> Work.child runtimeBranch
```

`Work.child` is a thin project helper selecting the usual progress type and
bundling the exact request/progress/worker handles. It does not select a model,
change effort, choose context or create a hidden collector per worker. Custom
progress and heterogeneous results remain available through the underlying API.

For simple observation, the owner installs one short collector with a useful
renderer. For this pass's richer case, it installs an integration actor whose
sources map the two results into a small project protocol. The actor should match
the engineering join, which need not match an admission batch.

```haskell
data Integration result where
  ApiDone     :: Either ResponseFailure (ResponseResult ApiCandidate)
              -> Integration ()
  RuntimeDone :: Either ResponseFailure (ResponseResult RuntimeCandidate)
              -> Integration ()
  Incorporated :: SourcePair -> IntegrationEvidence -> Integration ()
  View        :: (IntegrationState -> result) -> Integration result

let inputs =
      [ Actor.settlementSource apiResponse ApiDone
      , Actor.settlementSource runtimeResponse RuntimeDone
      ]
integration <- Actor.startActor
  (Actor.withSources inputs $ Actor.stateful "integration" integrationStep)
  initialIntegration
```

The state retains original receipts and outstanding pairs. Its handler decides
readiness using the project's contract. It sends a new exact integration packet
once, retains later distinct packets, and stops repeating handled history. The Sol
owner still decides how to merge and what the resulting-source checks establish.

```haskell
brief <- Actor.call integration (View integrationBrief)
Actor.cast integration (Incorporated selectedPair checkedIntegration)
```

`View` performs a typed projection inside the actor. Full selected evidence remains
available without making every brief return the entire history. `Incorporated` is
project evidence, not an acknowledgment that happens to authorize unrelated work.

## The exact continuation we need to simplify

The parent has already chosen a reviewer, source policy and scope. A candidate
result should therefore start the review directly in the local actor. Its result
returns to the same mailbox while the handler is free to process other events.

```haskell
type ReviewEffects = '[Replies, Actor, Notifications]

data ReviewFlow result where
  CandidateDone :: Either ResponseFailure (ResponseResult (Outcome Candidate))
                -> ReviewFlow ()
  ReviewProgress :: Candidate -> ProgressState WorkProgress -> ReviewFlow ()
  ReviewDone :: Candidate
             -> Either ResponseFailure (ResponseResult (Outcome ReviewDecision))
             -> ReviewFlow ()
  ViewReview :: (ReviewState -> result) -> ReviewFlow result

step :: ReviewState -> ReviewFlow result
     -> Eff (ActorLocal ReviewFlow ': ReviewEffects) (result, ReviewState)

step state (CandidateDone receipt) = do
  let retained = rememberCandidate receipt state
  case reviewInputFor selectedScope receipt of
    Left issue -> do
      sent <- sendMessage owner (issueBrief issue)
      pure ((), retainIssue issue sent retained)
    Right input -> do
      (response, progress) <- reviewAgain reviewer reviewLabel input
      self <- Actor.self
      forwarding <- Actor.forward self
        [ Actor.progressSource progress (ReviewProgress (reviewInput input))
        , Actor.settlementSource response (ReviewDone (reviewInput input))
        ]
      pure ((), rememberReview response progress forwarding retained)

step state (ReviewProgress candidate progress) =
  handleReviewProgress candidate progress state
step state (ReviewDone candidate receipt) =
  handleReviewResult candidate receipt state
step state (ViewReview project) = pure (project state, state)
```

The state, source-selection function and result handlers are ordinary project
Haskell. Use record syntax for substantial state. They preserve actual candidates,
attempts and source evidence; Rust does not learn a review-stage taxonomy.

`reviewAgain` is the existing typed request operation. The actor's newly selected
effect row makes it callable without an owner-row route callback. `Actor.self`
captures this actor's typed address. `Actor.forward` is owned composition over fixed
sources, with an inspectable handle and automatic finite completion. It does not
attach sources to the already-running review actor or add another scheduler.

The review-result handler can cast an actual typed packet to `integration`, follow
a repair edge the parent already declared, or message its Sol owner about a new
decision. Each repeated review is an exact new request to the same useful specialist.
There is no model relay solely to recover the result and issue the known next call.

Do not request a repair from the owner of a still-pending parent delivery and then
wait behind that delivery. A retained implementer is usable after its prior request
settles; otherwise return the repair decision to its owning model. Do not silently
pick a different source or infer acceptance from candidate/receipt hash equality.

The request may be admitted before `rememberReview` commits. Failure at that point
must preserve the exact operation and typed result under the existing owner; this
is a required failure test, not a request-label retry convention authored here.

## What becomes short

The normal native message says what the owner can act on: exact candidate/pair,
changed issue and selected result identity. The full receipt is queried only when
needed. Haskell-to-Haskell handoffs carry values, not formatted transcript excerpts.

Source progress stays lossless after attachment. The local actor derives compact
changes and an outstanding-work view; it does not repeatedly forward its cumulative
state up the tree. A newer candidate does not silently supersede another useful one.
Question remove/reopen and changed evidence at the same commit remain distinct.

When waiting, the owner can do independent engineering or end its turn. The
installed actor continues routing. No extra watch is required to make its terminal
receipt authoritative, and no stale watch notice needs a reassurance poll.

When integration is finished, the parent closes the collector and receives its
retained final state through one helper. The existing cleanup owner releases the
selected finished workers, retaining active/uncertain work and intentionally kept
specialists. A forwarder finishing does not retire a worker. Late provider anomalies
remain supervised independently of request observation.

## Ship this as the example

The implementation should replace the current review-continuation workaround with
this flow, not add a disconnected demonstration. Run its actual skill code blocks
with model-free workers, including changed source, failed forwarding, failure after
request admission, repeated review and retirement racing new work.

Keep the basic collection skill short. Load the actor-composition skill when a
model needs this loop. Stable imports, real signatures and small executable examples
should replace guessed APIs and whole binding inventories. Actor messages use the
minimum recoverable delta; human readability is secondary between agents.
