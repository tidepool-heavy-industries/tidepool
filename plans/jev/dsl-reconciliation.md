# DSL reconciliation: Fable's sketch against Astra's

Recorded 2026-09-16. Compares [dsl-sketch.md](dsl-sketch.md) (Fable) with
[dsl-astra-sketch.md](dsl-astra-sketch.md) (Astra), both written
independently from the same brief: one Servant-style record whose mode
parameter reads as the question surface or the typed answer. Where they
differ, this document takes a position and says why. Sol decides.

## Where they agree

Both sketches, without coordination, landed on:

- One schema record with a mode parameter; `Questions`/`Ask` and
  `Answers`/`Answer` as the two record modes; one effect operation from the
  first to the second.
- Heterogeneous alternatives as a second mode-interpreted record with one
  `Option`/`Alt payload` field per alternative, eliminated by an exhaustive
  handler record. Payloads never touch the wire.
- Score levels as a record whose declaration order is the rubric order,
  interpreted as descriptions on the way out and masses plus legend on the
  way back.
- `Each` for runtime-sized keyed collections and `Group` for nesting,
  elaborated into one flat question map and rebuilt by exact key.
- A dynamic homogeneous `Choose a` beside the static `Choice opts`, lowering
  to the same wire primitive.
- Wire keys for static alternatives defaulting to snake-cased selector
  names, so keys are semantic by naming fields well.
- Descriptions as position-checked JSON values, not a type parameter per
  description.
- A `given premise` combinator that renders the premise into instructions
  and refers to nothing in the same packet.
- A one-field schema (`Only endpoint`, `jev1`) for the tiny use.
- Policy (`accept`, `pick`, `margin`) as optional pure functions over full
  distributions, never inside the effect; no implicit 0.5, no automatic
  escalation.
- Structural obligations at compile time, dynamic bounds and response
  correspondence at checked constructors and the decoder, opaque validated
  types in between, `-Werror=missing-fields` for handler totality.
- A qualified operator rather than changing the agent contract's closed
  family.
- The Rust handler owning transport, credentials, and the configured model.

That is the design. The rest is detail, and most of the detail goes Astra's
way.

## Where they differ

### Modes inside alternative records

Fable: `Describe`, `Prob`, `Handle r`. Astra: reuse `Questions` for
authoring alternatives, with `Masses`, `Legend`, and `Handlers r` as
inspection modes only.

**Take Astra's.** Authors construct with two modes total and never meet the
others unless they inspect a distribution or eliminate a result. Smaller
surface, same power.

### Exits

Fable: exits are structural. `withExits [noMatch ..., deferToModel ...]`
adds them to a question; the answer has `Picked (Selected opts) | Exited
Exit`; handler records cover only real alternatives and `decide` returns
`Left exit`. Astra: an exit is an ordinary `Option` field
(`noUsefulPath :: Option ()`, `askModel :: Option Handoff`), covered by the
same exhaustive handler record; nothing requires a defer alternative.

**Take Astra's for static `Choice`, keep Fable's shape for dynamic
`Choose`.** For a static record the author already writes every alternative
and every handler; an exit as a field is honest to the wire, exhaustive by
the same mechanism, and carries whatever payload the handback needs
(`Handoff`). A second elimination path buys nothing. For `Choose a` the
dynamic candidates share one payload type and exits cannot be candidates of
type `a`; there the answer must distinguish a dynamic pick from a static
exit. Astra's `Candidates Edge :+ Exits` composition and Fable's
`Picked a | Exited Exit` are the same idea; the prototype should settle the
spelling. Elimination is `(a -> r)` for the pick plus a small handler record
for the exits.

Fable's claim that every packet should carry a `defer_to_model` exit
survives as authoring guidance in
[microprogram-patterns.md](microprogram-patterns.md), not as a DSL
requirement. Astra is right that the DSL must not mandate it.

### Coalescing equal descriptions

Fable proposed a `coalesce` helper merging alternatives with equal
descriptions. Astra: never automatically; two commands or spans may share a
summary and still be different continuations.

**Astra is right.** Equivalence is the author's to assert. If a helper
exists it takes an author-supplied equivalence and retains every original
payload under the merged key. The key-bias finding still stands; the fix is
better descriptions and deliberate naming, not silent merging.

### Pools

Fable: value-level named candidate sets. `candidates "edges" [...]` names
the set, it is serialized into state once, questions over it reference keys
with null descriptions, and conflicting contents under one name are a build
error. Astra: `withPool` introduces a request-scoped handle with
`chooseFrom pool` and `eachIn pool` builders; the handle cannot be used
with a different request; state is explicitly wrapped as `context` plus
`pools` so authored state paths are not silently rewritten.

**Lean Astra's, and prototype both on one example.** The scoped handle gives
a correspondence the compiler can check rather than a string it must
compare, and the explicit state wrapping is the right call: a request that
carries pools should say so in its state shape rather than having paths
injected under it. The cost is one binding form. The user earlier rejected a
mandatory continuation around every call; a pool binding is optional and
only appears when a request shares candidates across questions, so it does
not violate that. If the prototype shows `withPool` forces rank-N types into
ordinary notebook code, fall back to Fable's named sets, which have no
scoping and only a build-time consistency check.

Either way, the recorded pool experiment justifies the feature and a
comparison, not an assumption of equal judgment quality.

### Keys and paths

Fable: reject empty keys and keys containing the path separator at the
checked constructor. Astra: admit every provider-valid key, including
punctuation and the empty string, and use an injective segment encoding for
flattened paths.

**Take Astra's.** The brief was to admit every valid provider shape. An
injective encoding costs a few lines and removes a rule authors would trip
on with real symbol names and commit subjects.

### Model selection

Fable: `Model` as an argument to `jev`. Astra: `ConfiguredModel` by default,
resolved by the handler, with `UseModel name` available if operator policy
permits.

**Take Astra's.** Model tier is handler configuration under the subagent
handler precedent. An authored override is a policy switch, not a second
operation.

### Response shape

Astra's `Response` carries `answers`, `resolvedModel`, `usage`, and
`diagnostics` for arithmetic findings that do not invalidate structure.
Fable's review recommended the same split without naming the field.

**Take Astra's.** The diagnostics field is where the rounding finding lives
without becoming a rejection.

### Runner-up

Fable's `Picked a` always carries a runner-up. Astra: optional, since a
one-alternative Choice is valid.

**Astra's.** `Maybe`.

## The merged surface

```haskell
data Inspect mode = Inspect
  { next     :: mode J.:- J.Choice Routes
  , probe    :: mode J.:- J.Choose Probe            -- dynamic; exits handled separately
  , enough   :: mode J.:- J.Noul
  , urgency  :: mode J.:- J.Score Urgency
  , children :: mode J.:- J.Each Relevance
  , review   :: mode J.:- J.Group ReviewJudgment
  } deriving Generic

data Routes mode = Routes
  { followCaller :: mode J.:- J.Option Edge
  , useWitness   :: mode J.:- J.Option Witness
  , noUsefulPath :: mode J.:- J.Option ()
  , askModel     :: mode J.:- J.Option Handoff       -- the exit is a field
  } deriving Generic

response <- J.jev (J.request state (inspection input))
case response of
  Left err -> pure (ProviderUnavailable err)
  Right r -> case J.accept policy r.answers.next of
    Left doubt    -> pure (NeedsJudgment input.handoff doubt)
    Right chosen  -> J.match chosen Routes
      { followCaller = inspectEdge
      , useWitness   = pure . Located
      , noUsefulPath = \() -> pure (NeedOtherCandidates input.handoff)
      , askModel     = pure . HandBack
      }
```

Two authoring modes. Exits as fields for static records. `Choose` with a
separate exit handler. Pools by scoped binding, to be prototyped. Injective
path encoding. Configured model. Full distributions on every answer, policy
outside the effect, and the current model turn as the fallback.

## Astra's second pass, and what the prototype settled

Astra's follow-up positions: keep `Choice opts` and `Choose a`; value-level
pools with consistency compared on serialized descriptions, since payloads
may be functions; exits as optional convenience, with an ordinary sum payload
such as `Either Handoff Edge` always available instead; `given` as a value
combinator; and six mechanical holes to fix before compiling. Astra also
asked that the prose be frozen and the three decisive examples be compiled.

The prototype (formerly `jev-integration/haskell/proto/`, since extracted to
the `jev-dsl` package at `~/dev/jev-dsl`) compiled and ran under the
dev-shell GHC 9.12.2 with `-Wall -Werror`:

- **One `Questions` value crafts the request and parses the response.**
  `prepare` compiles `s Questions` to the flat wire map; `jev` retains that
  same value and decodes the wire answers into `s Answers` against it. A
  selection outside the submitted alternatives, a wrong answer kind, or an
  unexpected answer key is a `DecodeError`, never a Haskell value.
- **Data-family leaves work.** `Q e` and `A e` keep the endpoint recoverable,
  and one generic traversal pairs `Rep (s Questions)` with `Rep (s Answers)`.
  Compile walks the question representation alone; decode walks the pair.
- **The tiny use is one call.** `jev1 transport state (choose "..." groups
  exits)` then `pickOr handBack answer continue`. Both the library exit and an
  `Either Handoff Command` payload with no library exit hand back correctly,
  so Astra's point stands: structural exits are sugar, not a mechanism.
- **The heterogeneous record works end to end.** `Choice Routes` with the
  exit as an ordinary `Option Handoff` field, `Choose Command` with a library
  exit, `Score Urgency` with legend preserved as content, `Each Relevance`
  keyed by text including a key containing a dot, and `Group Sufficiency`,
  all in one record, prepared to nine flattened questions and decoded back.
- **The holes are closed.** Cardinality and exit-key collisions are checked
  at preparation; `Presence`/`Nullable` distinguish omitted, null, and
  content for instructions, Choice descriptions, and Noul criteria while
  Score levels are non-null `Content`; question paths use an injective
  escape rather than banning characters; the response envelope carries the
  resolved model, usage, and a diagnostics list; handler records are not
  scoped, and the `match` signature carries the constraint that pairs the
  question and handler representations.
- **Misuse produces understandable errors.** A handler record for a
  different alternatives record fails with "couldn't match `Other` with
  `Routes`"; a wrong payload type names the field; a missing handler is an
  error under `-Werror=missing-fields`; applying one result's selection to
  another's distribution fails on the scope; and a `Level` or `Option` in a
  question record fails with a custom message naming the right endpoint.

A second pass closed the places the first cut constrained rather than
empowered: full-form builders accept every instruction form; `optionKeyed`
overrides a static alternative's wire key with any provider-valid text;
`Scale` is a runtime-sized rubric beside the static `Score`; `Many` is a fully
dynamic heterogeneous question map; `Raw` sends any question object and
returns its answer unparsed; and `given` is a value-level premise prefix over
every question kind. The demo's complex cell uses all of them in one record,
keeps two mechanisms alive on a near-tie, gathers one observation each, and
resolves on a second packet. The library also rejects probability keys
outside the submitted alternative set.

Astra's review of the prototype found five gaps, all now closed and each
covered by a demo assertion: `given` wraps rather than merges, so omitted and
null instructions keep their premise and nested premises stay distinct;
duplicate overridden keys and duplicate exit keys are preparation errors,
and internal association uses original selector identity; decoding rejects
extra probability keys, altered legends, out-of-range or non-finite values,
and Scale now checks its legend, while sum drift is a diagnostic; `Raw` is
explicitly unchecked, outside the guarantee, and requires a raw answer
rather than re-encoding a typed one; `ranked` orders exits with candidates.
Astra's answers to the open questions are adopted: `Selected` retains the
exact alternatives; escaped `Many` ids are fine; the injected transport is
right for the prototype. Static Score counts are checked at preparation, not
compile time, and the claim now says so.

The extraction into `~/dev/jev-dsl` added the real codec against captured
exchanges: 253 recorded successes render byte-identically and decode, and
every recorded rejection is inexpressible, rejected at `prepare`, or
provider-decided. Not yet prototyped: pools shared across two Choices and an
`Each`, the third decisive example.

## Feasibility sequence

Astra's order, with Fable's checks folded in:

1. Compile one complete static record with `Choice`, `Noul`, `Score`,
   `Group`, and heterogeneous elimination through an exhaustive handler
   record. Check: the four existing compile-fail fixtures still fail, plus
   a fixture applying one result's selection to another's masses.
2. Add `Choose a` with static exits. Check: a dynamic pick and a static exit
   eliminate through different handler shapes, and a response naming a key
   outside the submitted set is a decoder error, not a Haskell value.
3. Prove pool association on one request with two Choices and one `Each`
   over the same candidates, under both `withPool` and named sets. Check:
   one copy of descriptions in state, correct rebuilding of all three
   answers, and a handle from another request rejected at compile time.
4. Prove recursive `Each` elaboration over a finite value without expanding
   the type. Check: flattened keys round-trip through the injective
   encoding for keys containing separators, punctuation, and the empty
   string.
5. Codec and live tests against the observed contract profile, including
   omitted-versus-null instructions and all Noul criteria forms.

No engine change is needed for any step; all five run under plain GHC and,
for step five, the research crate's transport.
