# Jev DSL sketch: one record, two modes

Recorded 2026-09-16. A proposal, not compiled. Names are illustrative and
Sol owns the final abstraction. The aim is the user's formulation: one
Servant-style record whose mode parameter reads as the question surface in
one interpretation and as the typed answer in the other, so a program writes
the record once and gets both.

## The shape in one screen

```haskell
data Investigation mode = Investigation
  { mechanism  :: mode :- Choice Mechanisms            -- static, heterogeneous payloads
  , nextProbe  :: mode :- Choose Probe                 -- dynamic, homogeneous payloads
  , sufficient :: mode :- Noul
  , urgency    :: mode :- Score Urgency
  , commits    :: mode :- Each CommitId CommitJudgment -- runtime-sized keyed collection
  , review     :: mode :- Group ReviewJudgment         -- nested reusable record
  } deriving Generic

data Mechanisms mode = Mechanisms
  { actorRedelivery      :: mode :- Alt RetryEdge
  , inboxDoubleAdmission :: mode :- Alt InboxSpan
  , projectionInvocation :: mode :- Alt ProjectionSpan
  } deriving Generic

data Urgency mode = Urgency          -- declaration order is level order
  { background :: mode :- Level
  , nextCheckpoint :: mode :- Level
  , blocksNextAction :: mode :- Level
  , invalidatesWork :: mode :- Level
  } deriving Generic
```

Then, in a cell:

```haskell
judged <- jev model (record world) Investigation
  { mechanism  = choice "Which mechanism directly explains the second callback?"
                   Mechanisms
                     { actorRedelivery      = alt retryDesc retryEdge
                     , inboxDoubleAdmission = alt inboxDesc inboxSpan
                     , projectionInvocation = alt projDesc projSpan
                     }
                   `withExits` [noMatch "The observations do not distinguish a mechanism"]
  , nextProbe  = choose "Which focused query best discriminates the remaining mechanisms?" probes
                   `withExits` [deferToModel "Choosing needs a design preference"]
  , sufficient = noul "Does the supplied evidence establish the mechanism?"
  , urgency    = score "How costly is delaying this to the next wake?" urgencyLevels
  , commits    = each recent $ \c -> CommitJudgment
                   { touchesNotebook = noul ("Does " <> c.subject <> " plausibly modify notebook display?")
                   , isBehaviorChange = noul ("Is " <> c.subject <> " a behavior change?") }
  , review     = group ReviewJudgment { ... }
  }

case judged of
  Left err -> handBack err
  Right a -> do
    edge <- decide a.mechanism Mechanisms
      { actorRedelivery      = \e -> followRetry e
      , inboxDoubleAdmission = \s -> readSpan s
      , projectionInvocation = \s -> readSpan s
      }
    ...
```

`judged :: Either JevError (Investigation Answer)`. The same record value
shape, interpreted the other way.

## Modes

Two record modes and three alternative modes.

| Mode | Applies to | A field reads as |
|---|---|---|
| `Ask` | question records | the authored question with instructions, alternatives, and exits |
| `Answer` | question records | the typed answer: selection with payload, distribution, confidence, or a typed exit |
| `Describe` | alternative and level records | description plus local payload |
| `Prob` | alternative and level records | a probability |
| `Handle r` | alternative records | `payload -> r` |

The interpretation is a closed family with a `TypeError` fallthrough, like
the agent contract's, but the leaves are data families so the endpoint stays
recoverable from the leaf value. That is what lets one generic traversal
compile `schema Ask` to the wire and rebuild `schema Answer` from it.

```haskell
type family mode :- endpoint where
  Ask      :- e = Q e
  Answer   :- e = A e
  Describe :- Alt p   = Option p        -- (Description, p)
  Prob     :- Alt p   = Double
  Handle r :- Alt p   = p -> r
  Describe :- Level   = Description
  Prob     :- Level   = Double
  mode :- e = TypeError (...)

data family Q endpoint    -- the question leaf
data family A endpoint    -- the answer leaf
```

Whether this operator is the agent contract's `:-` extended into an open
family or a Jev-qualified sibling is Sol's decision; the sketch assumes a
qualified operator to avoid touching the closed family.

## Endpoints

### `Choice opts`: static, heterogeneous

```haskell
data instance Q (Choice opts) = ChoiceQ
  { instructions :: Instructions
  , alternatives :: opts Describe
  , exits        :: [Exit] }

data instance A (Choice opts) = ChoiceA
  { outcome      :: Outcome opts          -- Picked (Selected opts) | Exited Exit
  , distribution :: opts Prob
  , exitMass     :: Map ExitKey Double
  , confidence   :: Double }
```

`Selected opts` is the existential from the current sketch: it knows which
field won and can apply the matching handler from an `opts (Handle r)` record
or project the matching probability from `opts Prob`, with a phantom scope
that prevents mixing results.

Wire keys for static alternatives are the snake-cased selector names, so the
model sees `actor_redelivery`, `inbox_double_admission`,
`projection_invocation`. Keys are semantic by naming fields well.

### `Choose a`: dynamic, homogeneous

```haskell
data instance Q (Choose a) = ChooseQ
  { instructions :: Instructions
  , candidates   :: Candidates a
  , exits        :: [Exit] }

data instance A (Choose a) = ChooseA
  { outcome      :: Outcome' a            -- Picked (Candidate a) | Exited Exit
  , ranked       :: [(Candidate a, Double)]
  , exitMass     :: Map ExitKey Double
  , confidence   :: Double }
```

`Candidates a` comes only from a checked constructor:

```haskell
candidates :: Text -> [(CandidateKey, Description, a)] -> Either BuildError (Candidates a)
```

It rejects empty, duplicate, or separator-bearing keys, and counts above
255 minus the exits. The first argument names the set; a named set is
serialized into state once under `candidates.<name>` and every question over
it references keys with null descriptions. Two sets with one name and
different contents are a build error. This is the pool position at value
level: no type-level field references, and sharing is just reusing the same
value in several fields.

`Candidate a` carries the key, the description, and the payload, and is the
value `pick` returns, so the next command is built from the retained payload
and never from the key.

### `Noul`, `Score levels`

```haskell
data instance Q Noul = NoulQ { instructions :: Instructions, criteria :: Maybe NoulCriteria }
data instance A Noul = NoulA { yes :: Double }

data instance Q (Score levels) = ScoreQ { instructions :: Instructions, levels :: levels Describe }
data instance A (Score levels) = ScoreA
  { expectation :: Double, distribution :: levels Prob, legend :: levels Describe, confidence :: Double }
```

A level record's declaration order is the rubric order. The legend comes back
as the record it was sent as; nothing is stringified.

### `Each k s`, `Group s`

```haskell
newtype instance Q (Each k s) = EachQ (Map k (s Ask))
newtype instance A (Each k s) = EachA (Map k (s Answer))
newtype instance Q (Group s)  = GroupQ (s Ask)
newtype instance A (Group s)  = GroupA (s Answer)
```

`each :: (Ord k, WireKey k) => [(k, x)] -> (x -> s Ask) -> Q (Each k s)`
builds the collection. Flattening produces `commits.<key>.touches_notebook`;
`WireKey` rejects the separator. Answers are rebuilt by exact key lookup, not
by parsing paths, so the only invariant is that flattened keys are unique,
which the separator rule guarantees.

## Exits are structural

An exit is not another alternative. It is a typed constructor of the
outcome, so handler records over real alternatives stay exhaustive and the
handback is a separate case:

```haskell
data Exit = NoMatch Description | DeferToModel Description | Custom ExitKey Description

data Outcome opts = Picked (Selected opts) | Exited Exit
```

On the wire an exit is a criteria key (`no_match`, `defer_to_model`) with its
description, so the model can choose it. On the way back, a selected exit key
becomes `Exited`, never `Picked`, and its mass is reported beside the
alternative distribution. `withExits` is the only way to add them, and a
`Choice` with no exits is legal for closed questions that always have an
answer.

`deferToModel` is how the tree says "not mine". The cell's outcome type turns
it into `NeedsJudgment` with the evidence in scope.

## Consuming answers

```haskell
decide   :: A (Choice opts) -> opts (Handle r) -> Either Exit r
pick     :: Policy -> A (Choose a) -> Picked a           -- winner, runner-up, masses, confidence, or exit
margin   :: A (Choice opts) -> Double                     -- winner minus runner-up
yes      :: Threshold -> A Noul -> Maybe Bool             -- Nothing near 0.5
level    :: A (Score levels) -> (Double, levels Prob)
pickOr   :: (Exit -> Eff es r) -> Policy -> A (Choose a) -> (a -> Eff es r) -> Eff es r
```

`Picked a` carries the winner and runner-up with masses so policy sees
near-ties. `decide` returns `Left exit` on an exit rather than forcing every
handler record to include one. `pickOr handBack` is the tiny use.

## The effect

One operation:

```haskell
jev :: (Member Jev es, Schema s)
    => Model -> State -> s Ask -> Eff es (Either JevError (s Answer))
```

`Schema s` is the generic class: it compiles `s Ask` to the wire question
map plus any named candidate sets into state, and rebuilds `s Answer` from
the validated wire answers. `State` is built by `record` over an ordinary
`Generic` record or by literal builders, and refuses outer scalars and null.
The Rust handler validates structure against the submitted request and
returns a typed error; Haskell never sees an answer whose key or kind does
not match what it sent.

`JevError` covers build errors surfaced late (should not happen; construction
is checked), transport and provider failures, and structural mismatches.
Semantic disappointments are not errors; they are exits and distributions.

## Premise-prefixed questions

```haskell
  , checkIfRetry :: mode :- Given "mechanism is actor_redelivery" (Choose Check)
```

`Given premise e` wraps any endpoint. In `Ask` mode it prefixes the rendered
premise to the instructions; in `Answer` mode it is transparent. The premise
is a value at construction (`given "..." (choose ...)`), with the type-level
symbol optional sugar for static records. Policy consumes the answer whose
premise won.

## Handback and outcome types

The library does not dictate a cell's outcome type, but it supplies the
common one and the helper that builds it from any exit:

```haskell
data Outcome evidence resolved
  = Resolved resolved evidence Trace
  | NeedsJudgment Inquiry evidence [Alternative] Trace
  | Consult Owner Question evidence Trace

handBack :: Inquiry -> evidence -> Exit -> Eff es (Outcome evidence r)
```

`Trace` references the retained packets, responses, distributions, and
candidate mappings in the journal. The resuming model receives the outcome
value with everything else still bound in the session.

## What the compiler checks

- Only endpoints of the closed family appear in a schema record; anything
  else is a `TypeError` naming the field.
- A handler record for `decide` must cover every alternative of the
  question's alternatives record, with the right payload type per field.
- A `Selected` from one result cannot be applied to another result's
  probabilities or handlers.
- Level records and alternative records must be single-constructor products
  of `Level` or `Alt` fields.
- `Score` level count is checked at the type level for static records (1 to
  10 fields); `Choose` cardinality is a runtime check in `candidates`.

What only runtime checks: candidate key validity, named-set consistency,
flattened key uniqueness under `Each`, and every property of the response.

## The two ends of the range

Tiny:

```haskell
group <- pickOr handBack policy =<< jev1 model world (choose "Which diagnostic explains the failure?" groups)
capture (readSpan group.span)
```

`jev1` wraps a single question in a one-field schema so the smallest use
needs no record declaration.

Rich: the `Investigation` record above, three packets in one cell, competing
mechanisms kept alive by `margin`, and `NeedsJudgment` returned with the
edges pool, the two surviving mechanisms, and their discriminating
observations when the third packet still splits.

## Open points for Sol

- Operator: extend the agent contract's family or a qualified sibling.
- Whether `Choose a` and `Choice opts` should unify under one endpoint with
  the alternatives record as a type-level `Static opts | Dynamic a` choice,
  or stay two endpoints. Two reads more clearly; one has fewer instances.
- Whether named candidate sets should also be first-class `Pool` fields for
  schemas that want the pool visible in the record type, or whether the
  value-level naming is enough.
- Whether `Given` should exist at the type level at all.
- The exact `Policy` vocabulary for `pick`: margin floor, confidence floor,
  and what counts as a near-tie, all illustrative until the corpus speaks.
