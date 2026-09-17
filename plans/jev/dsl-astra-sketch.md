# Jev records: independent DSL sketch

2026-09-16. Proposal for comparison with Fable's parallel sketch. Code below is
illustrative Haskell, not a compiled module or an implemented API. The existing
`jev-integration/haskell/Sketch.hs` establishes some of the underlying mechanics;
it does not establish the new interfaces proposed here.

## The authored surface

Define a record once. Its mode says whether its fields are questions or answers.
Question wording and structured descriptions are ordinary values. Types describe
the answer shape and retain the association with local payloads.

```haskell
data Inspect mode = Inspect
  { next     :: mode J.:- J.Choice Routes
  , enough   :: mode J.:- J.Noul
  , urgency  :: mode J.:- J.Score Urgency
  , children :: mode J.:- J.Each Relevance
  } deriving Generic

data Relevance mode = Relevance
  { useful        :: mode J.:- J.Noul
  , contradicts   :: mode J.:- J.Noul
  } deriving Generic

data Routes mode = Routes
  { followCaller :: mode J.:- J.Option Edge
  , runCheck     :: mode J.:- J.Option Check
  , useWitness   :: mode J.:- J.Option Witness
  , noUsefulPath :: mode J.:- J.Option ()
  , askModel     :: mode J.:- J.Option Handoff
  } deriving Generic

data Urgency mode = Urgency
  { background   :: mode J.:- J.Level
  , checkpoint   :: mode J.:- J.Level
  , blocked      :: mode J.:- J.Level
  , invalidating :: mode J.:- J.Level
  } deriving Generic
```

`Routes` represents a heterogeneous sum using record syntax: request mode
supplies all alternatives, answer mode selects one, handler mode eliminates it.
`Inspect` is a product: every question gets an answer. These are distinct
interpretations; neither requires authors to write a type-level list.

```haskell
inspection :: Inputs -> Inspect J.Questions
inspection input = Inspect
  { next = J.choice
      (J.object
        [ ("question", json "Which available continuation advances the inquiry?")
        , ("inquiry", json input.inquiry)
        , ("scope", json "Select only from evidence supplied in state.")
        ])
      Routes
        { followCaller = J.option (describe input.edge) input.edge
        , runCheck     = J.option (describe input.check) input.check
        , useWitness   = J.option (describe input.witness) input.witness
        , noUsefulPath = J.option "None of these continuations is useful." ()
        , askModel     = J.option
            "Choosing requires a judgment beyond the supplied evidence or scope."
            input.handoff
        }
  , enough = J.noul "Does the supplied evidence answer the inquiry?"
  , urgency = J.score "What is the consequence of waiting?" Urgency
      { background   = J.level "No current action depends on this."
      , checkpoint   = J.level "Useful at the next ordinary checkpoint."
      , blocked      = J.level "A worker cannot take its next action."
      , invalidating = J.level "Continuing would invalidate ongoing work."
      }
  , children = J.each input.children $ \child -> Relevance
      { useful = J.noul (relevanceInstruction input.inquiry child)
      , contradicts = J.noul (contradictionInstruction input.inquiry child)
      }
  }
```

Here `input.children` is a checked keyed collection. `describe`, `json`, and
instruction-building functions above are application functions; structured values
are not constrained to a preselected vocabulary such as question/focus/scope.
`OverloadedStrings` supplies text entries in positions where text is legal.

```haskell
reply <- J.jev (J.request state (inspection input))
case reply of
  Left err -> pure (ProviderUnavailable err)
  Right response -> do
    let answers = response.answers       -- Inspect J.Answers
    case J.accept policy answers.next of
      Left doubt -> pure (NeedsJudgment input.handoff doubt)
      Right selected -> J.match selected Routes
        { followCaller = inspectEdge
        , runCheck     = executeCheck
        , useWitness   = pure . Located
        , noUsefulPath = \() -> pure (NeedOtherCandidates input.handoff)
        , askModel     = pure . HandBack
        }
```

All handlers return the same effectful outcome. Matching invokes exactly the
selected handler, after policy accepts the answer. Handback retains original
evidence and alternatives; Jev does not generate a reason or a new question.
The author supplies those values or derives them from the selected branch.

`accept` is optional policy, not part of the effect. Full distributions remain
available before and after it. There is no implicit 0.5 cutoff, no automatic
escalation, and no requirement that every packet contain a defer alternative.

## Mode interpretation

The qualified operator avoids changing `Tidepool.Agent.Contract`'s closed family.
The record syntax stays familiar even if a cell imports both libraries.

| Endpoint | Questions | Answers | Additional interpretation |
| --- | --- | --- | --- |
| `Noul` | `NoulQuestion` | `NoulAnswer` | — |
| `Choice options` | `ChoiceQuestion options` | `ChoiceAnswer options` | — |
| `Score levels` | `ScoreQuestion levels` | `ScoreAnswer levels` | — |
| `Group schema` | `schema Questions` | `schema Answers` | — |
| `Each schema` | `Keyed (schema Questions)` | `Keyed (schema Answers)` | — |
| `Option a` | `Alternative a` | not a question field | `Masses`: probability; `Handlers r`: `a -> r` |
| `Level` | non-null structured description | not a question field | `Masses`: probability; `Legend`: structured description |

Use `Questions` inside alternatives as well as the outer record: authors need
only two modes for ordinary construction and consumption. `Masses`, `Legend`,
and `Handlers r` are interpretations for inspecting or eliminating results.
Unsupported mode/endpoint pairs produce a focused `TypeError`.

```haskell
type family Field mode endpoint where
  Field Questions Noul             = NoulQuestion
  Field Answers Noul               = NoulAnswer
  Field Questions (Choice opts)    = ChoiceQuestion opts
  Field Answers (Choice opts)      = ChoiceAnswer opts
  Field Questions (Score levels)   = ScoreQuestion levels
  Field Answers (Score levels)     = ScoreAnswer levels
  Field Questions (Option a)       = Alternative a
  Field Masses (Option a)          = Probability
  Field (Handlers r) (Option a)    = a -> r
  Field Questions Level            = Content
  Field Masses Level               = Probability
  Field Legend Level               = Content
  -- Group, Each, and explanatory TypeError cases omitted here.
```

Descriptions use position-checked JSON values rather than a type parameter per
description. Applications can retain typed description records and encode them
with a checked codec. The result generally needs typed payload identity, not the
Haskell source type of the description. This removes one parameter from every
`Option` without losing structured provider inputs.

## One operation and its boundary

```haskell
jev :: (Member Jev effects, Schema schema)
    => Request schema
    -> Eff effects (Either JevError (Response schema))

-- Constructors private; request construction establishes structural validity.
-- Static schemas can discharge static obligations at compilation.
-- Runtime collections require checked construction/preparation.
data Response schema = Response
  { answers       :: schema Answers
  , resolvedModel :: ModelName
  , usage         :: Usage
  , diagnostics   :: [ArithmeticDiagnostic]
  }
```

`Request schema` retains the authored questions and local alternatives, alongside
their prepared wire representation. Decoding must use this exact request's
retained alternatives. It must never resolve provider labels against a later
candidate pool. Exactly one effect operation submits state and questions; the
typed wrapper performs preparation and reconstruction around that operation.

The request includes model selection, normally `ConfiguredModel`, resolved by
the handler. An explicit `UseModel ModelName` can preserve the provider's model
option if operator policy permits it. Whether authored model overrides belong in
the initial API is a design choice, not a reason to add another operation.

Provider failure and `NeedsJudgment` are separate: one is transport/protocol
failure; the other is successful application control flow.

## Full structured inputs, including awkward distinctions

Follow the live [advanced structures](https://docs.typesafe.ai/primitives/advanced)
and [API](https://docs.typesafe.ai/api) references, with the disagreements recorded
in [CONTRACT.md](../../jev-integration/CONTRACT.md). The advanced page still permits
null Score levels while our live probes rejected them; the initial profile follows
the observed contract. Do not silently claim these sources are identical.

Conceptual constructors:

```haskell
data Content = Text Text | Object (Map Text Value) | Array [Value]
data Nullable a = Null | NonNull a
data Presence a = Omitted | Present a

type Instructions = Presence (Nullable Content)
type Description  = Nullable Content

data NoulCriteria = NoulCriteria
  { yes :: Presence Description
  , no  :: Presence Description
  }
```

State is `Content`; nested `Value`s admit all JSON forms. Choice descriptions
admit null; Score levels use `Content`; instructions preserve omitted versus
explicit null. Noul criteria preserve omitted, null, empty object, and independently
omitted/null/structured true and false descriptions. Convenience `noul`, `choice`,
and `score` builders sit over total primitive constructors exposing these forms.
Examples use `J.object` for non-null content, lifted into nullable/optional positions
by the builders. Arbitrary `ToJSON` values require a checked outer-shape conversion.

For the observed profile: static Choice records have 1–255 alternatives and
static Score records 1–10 levels. Generic field order determines Score order;
changing declaration order changes its meaning. Dynamic rubrics and candidate
sets use bounded smart constructors. Empty dynamic `Each` is legal inside a
larger packet; a prepared packet with no questions is rejected locally.

Do not ban provider-valid punctuation or empty Choice keys just to simplify the
DSL. Question paths use an injective segment encoding; dynamic keys round-trip
without separator collisions. Static Choice keys default to semantic selector
names, with collision checking after any normalization and an explicit wire-name
override. Question IDs are association keys; they do not replace instructions.

## Results preserve uncertainty and payloads

`NoulAnswer` exposes probability of yes, not a coerced Bool. `ScoreAnswer levels`
exposes expectation, `levels Masses`, `levels Legend`, and provider confidence.
It does not round expectation into an action. Policy may inspect tail mass instead
of the mean when high-impact levels matter.

`ChoiceAnswer options` retains a nonempty ranked collection of selected
alternatives with masses, plus `options Masses` and provider confidence. Runner-up
is optional because one-option Choices are valid. Selecting or inspecting a
ranked alternative is pure; executing it requires matching against handlers.

Keep the existing sketch's hidden existential scope and nominal roles whenever
an API accepts separate selection and distribution arguments. Otherwise a caller
could accidentally compare a selection from one invocation with another's masses.
A bundled ranked entry can expose its own mass without exposing the scope at all.
Ordinary notebook use should not require rank-N annotations.

Equal descriptions do not prove equal continuations: two commands or source spans
may share a summary. Never automatically coalesce alternatives by description
equality. An optional helper needs author-supplied semantic equivalence and must
retain the original payloads.

## Dynamic candidates and pools

Static records cover heterogeneous alternatives. Runtime tool results require a
homogeneous candidate constructor as well:

```haskell
data Locate mode = Locate
  { target :: mode J.:- J.Choose Edge
  }

-- Checked candidate collection; each candidate has key, description, payload.
Locate { target = J.choose instruction edges }
-- Answers mode yields Ranked Edge; it does not yield a label needing lookup.
```

`Choose a` and `Choice options` lower to the same Choice primitive. For dynamic
alternatives plus static exits, support composition such as
`Choice (Candidates Edge :+ Exits)`, whose elimination distinguishes the dynamic
payload from the static handlers. This composition needs a feasibility prototype;
it is not established by the old sketch.

I would keep shared pools as request-level bindings initially, rather than fields
masquerading as questions. `withPool` introduces a scoped handle, arranges one copy
of descriptions in state, and supplies `chooseFrom pool` / `eachIn pool` builders.
The compiler lowers references to that pool's state path; callers cannot combine
a handle with a different request. State wrapping must be explicit: a pool-bearing
request has named `context` and `pools`, so existing authored paths are not silently
rewritten. Ordinary requests preserve string/array/object state exactly.

This is the main alternative to Fable's `Pool` endpoint proposal. A field-based
pool could be nicer if it gives stronger correspondence without fragile sibling
name lookup. Compare both with an actual two-Choice-plus-Each example before
choosing. The recorded pool experiment motivates this feature, not an assumption
of equal judgment quality or proportional token savings.

## Groups, premises, and the tiny use

`Group` and `Each` are structural elaboration into one question map, not more
effect operations. A `given premise` combinator renders the premise into each
affected instruction. It does not refer to an answer in the same packet. Runtime
premises should remain values; type-level string literals are optional sugar.

The tiny use can be a generic one-field schema:

```haskell
data Only endpoint mode = Only { value :: mode J.:- endpoint }

-- Schematic; provider and preparation errors handled by the surrounding cell.
response <- J.jev (J.request state (Only (J.choose instruction commands)))
-- Policy can hand back or execute a retained Cmd; labels never become commands.
```

The same operation supports this and a whole investigation record. Hylo
coalgebras/algebras merely construct different question values and consume their
answers. No recursion scheme, scheduler, escalation framework, or fixed policy
belongs in the primitive DSL.

## What the types can promise

Static endpoint shape, payload types, handler result agreement, schema cardinality,
and (with the established warning policy) missing record fields are compile-time
obligations. Dynamic uniqueness, bounds, nonempty flattened packets, JSON outer
shapes, and response correspondence are checked at their owning constructors or
decoder, then represented by opaque validated types.

`Generic` derivation needs an explicit schema grammar: products of named fields,
known endpoints, valid alternatives/levels, and no unsupported sum/unnamed nodes.
Recursive `Each` schemas must be validated over the finite request value, without
infinitely expanding a recursive type to count questions.

This is ordinary Haskell totality: bottom remains possible, and missing fields
need `-Werror=missing-fields`. The DSL cannot statically prove that evidence is
sufficient or Jev's judgment is true. It can keep a Noul probability from silently
becoming a fact or an actor's authority-bearing reply.

## Decisions to compare with Fable

1. Two primary modes with `Option a`, versus a distinct descriptions mode and
   `Option a description`. I favor the smaller authored surface.
2. Pool bindings versus `Pool` fields. Demonstrate request scoping and two
   questions sharing one dynamic pool before committing to either.
3. `Choice options` plus `Choose a`, versus one candidate-shape abstraction that
   unifies both. Prefer the version with clearer notebook errors and fewer visible
   parameters, provided both lower through one implementation.
4. Explicit model override policy; transport and credentials remain handler-owned.

Next feasibility work: compile one complete static record with all three
primitives and heterogeneous elimination; add dynamic candidates with static exits;
then prove pool association and recursive Each elaboration. Codec/live tests follow
that shape. No production engine changes are required to compare the sketches.

Verification for this document: reviewed against the local contract and existing
GHC sketch, and checked whitespace. No new code compiled and no live inference
calls made. The TypeSafe skill informed position-specific inputs, explicit premises,
and keeping probability policy separate from primitive results.
