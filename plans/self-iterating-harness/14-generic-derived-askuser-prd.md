# PRD — Generic-derived `askUser`

**Status:** proposed (2026-08-08)  
**Owner:** self-iterating harness / operator interaction surface  
**Replaces:** the agent-authored applicative `Form` builder in
`Tidepool.Form`

## Summary

An agent that needs human input describes the answer as an ordinary Haskell
algebraic data type and derives `Generic`:

```haskell
data Environment = Development | Staging | Production
  deriving (Generic)

data DeployRequest = DeployRequest
  { service       :: Text
  , environment   :: Environment
  , replicas      :: Int
  , runMigrations :: Bool
  , releaseNote   :: Maybe Text
  } deriving (Generic)
```

Then:

```haskell
request <- askUser @DeployRequest
```

That is the entire agent-facing form API.

`askUser` traverses `Rep DeployRequest` twice: once to derive a structural
form description, and once to rebuild a typed `DeployRequest` from the
operator's structural answer. It does not use `FromJSON`, `ToJSON`, JSON
Schema, an applicative form builder, or an agent-authored widget description.

The type is the form. Haskell's ordinary sum/product structure is the schema.

## Product thesis

Tidepool's strongest surface is Haskell that models already know. A fresh
model can reliably write:

- a record for a product;
- constructors for alternatives;
- a nullary sum for a finite choice;
- `Maybe a` for optionality; and
- primitive leaves such as `Bool`, `Int`, and `Text`.

Asking it to restate that same shape using form combinators, parser
combinators, HTML, or JSON Schema creates a second artifact that can drift
from the answer type. Generic derivation makes that duplication unnecessary:

```text
                         ┌─> structural FormSpec ─> operator UI
ordinary ADT ─> Generic ─┤
                         └─< structural answer  ─< operator submission
                                      │
                                      └─> typed ADT value
```

The only Tidepool-specific idea the model must learn is `askUser @T`.

## Goals

1. Make a typed human prompt require one ordinary type declaration and one
   `askUser @T` expression.
2. Require only `deriving (Generic)` on agent-defined types.
3. Support arbitrary finite sums and products whose leaves are supported core
   types.
4. Preserve a statically typed value in the Haskell continuation without a
   JSON codec round trip.
5. Let the renderer choose controls and layout from structural meaning.
6. Make unsupported or ambiguous shapes fail at compile time with concise,
   corrective `TypeError`s.
7. Reuse the existing `AskUser` effect, suspension routing, operator gate,
   blocking behavior, and bounded re-prompt loop.
8. Keep both the prompt cost and the generated Haskell surface tiny.

## Non-goals

- Agent control over radio versus select, slider versus number input, layout,
  grouping, styling, or other widget details.
- Compatibility with aeson's generic encoding.
- Materializing JSON Schema in the Haskell program.
- Defaults, help text, placeholders, bounds, regexes, or annotations in v1.
- Cross-field semantic validation in v1.
- Runtime-supplied choice sets in the zero-argument `askUser @T` API.
- Recursive/infinite types in v1.
- Pretending that lists are ordinary finite generic sums. Lists require a
  repeat-editor contract and receive a deliberate container implementation
  later.

## Primary user experience

### Simple record

```haskell
data Clarification = Clarification
  { question :: Text
  , blocking :: Bool
  , context  :: Maybe Text
  } deriving (Generic)

answer <- askUser @Clarification
```

### Choices

```haskell
data Priority = Low | Normal | Urgent
  deriving (Generic)

data Clarification = Clarification
  { question :: Text
  , priority :: Priority
  } deriving (Generic)

answer <- askUser @Clarification
```

Constructor names are already the canonical keys for a type-defined choice.
No `[(Text, Priority)]` mapping is needed.

### Sums with payloads

```haskell
data Destination
  = Local
  | Ssh { host :: Text, port :: Int }
  | Container { image :: Text }
  deriving (Generic)

destination <- askUser @Destination
```

The UI first asks for `Local`, `Ssh`, or `Container`, then presents the chosen
constructor's payload. `Local` has no payload. Constructor names key the outer
sum; record selectors key each payload product.

### Nested sums

```haskell
data Speed = Fast | Thorough deriving (Generic)
data Scope = CurrentFile | Workspace deriving (Generic)

data Operation
  = Search Scope
  | Analyze Speed
  deriving (Generic)

operation <- askUser @Operation
```

The outer keys are `Search` and `Analyze`; the selected payload contributes
the inner `Scope` or `Speed` choice. This is an “enum of enums” without an
extra mapping language: constructor paths are the keys.

Domain-named constructors produce good forms. Generic containers such as
`Either Scope Speed` are mechanically possible but yield the weak outer keys
`Left` and `Right`; the agent should define a tiny domain sum when labels
matter.

## Agent-facing documentation

The entire advertised contract should fit in one paragraph:

> `askUser @T` presents a human form and returns `T`. Define `T` using ordinary
> records and constructors, derive `Generic` for it and any nested custom
> types, and end fields in `Text`, `Int`, `Double`, or `Bool`. Constructors are
> choices, record fields are named inputs, and `Maybe a` is optional.

One compact example may follow. Do not advertise implementation classes,
wire values, JSON, or the old field constructors.

## Structural language

The derivation recognizes a deliberately small algebra:

```text
Form a := Text
        | Int
        | Double
        | Bool
        | Optional (Form a)
        | Product ConstructorKey [FieldKey × Form]
        | Sum TypeKey [ConstructorKey × Form]
        | Unit
```

This algebra is recursive. A product may contain sums, a sum constructor may
carry a product, and either may nest until it bottoms out in supported leaves.

### Core leaves and blessed containers

| Haskell type | Structural meaning | Typical control |
|---|---|---|
| `Text` | string leaf | single-line text input |
| `Int` | bounded integral leaf | integer input |
| `Double` | numeric leaf | numeric input |
| `Bool` | boolean leaf | checkbox/toggle |
| `Maybe a` | optional supported shape | optional/nullable control or group |
| `()` | unit/no payload | no control |

`Bool`, `Maybe`, and `()` have custom core implementations even though they
also have generic representations. Their user-facing meanings take priority
over their implementation structure: `Bool` is not rendered as an enum and
`Maybe a` is not rendered as a `Nothing`/`Just` constructor picker.

`String` is intentionally unsupported. Tidepool is Text-first, and `[Char]`
would collide with the future collection meaning of lists.

### Products

- A record product uses exact selector names as stable field keys.
- Declaration order is presentation order.
- A one-field `newtype` or constructor remains a named structural boundary;
  the renderer may visually collapse it while preserving its decode path.
- Positional constructor fields are supported mechanically with stable
  one-based keys (`"1"`, `"2"`, …), but receive deliberately generic display
  labels. Agents should prefer record syntax whenever field meaning matters.
- Tuples follow the same positional-product rule.

### Sums

- Each constructor name is the stable key for one branch.
- A constructor's fields are the branch payload.
- An all-nullary sum is an enum and may render as one compact choice control.
- A payload-bearing sum renders as a discriminated choice followed by the
  selected branch's nested form.
- Constructor declaration order is option order.
- The generic representation's balanced `:+:` tree is an implementation
  detail; it must not leak into keys or ordering.

### Optionality

`Maybe a` is a blessed container rather than an ordinary generic sum:

- absent submits the structural `None`/null form;
- present submits the ordinary structural answer for `a`;
- optional products and sums are valid;
- nested `Maybe (Maybe a)` is rejected because its three states cannot be
  communicated cleanly by a normal optional control.

## Where keys come from

No parallel `[(String, x)]` table is required for choices already defined by
the type:

- datatype name: type key/title;
- constructor name: sum branch or enum option key;
- selector name: named product field key;
- field position: fallback key for positional products.

These keys are structural identities used for round-trip decoding. The Rust
renderer may humanize `NeedsReview` to “Needs review” and `releaseNote` to
“Release note”, but display transformations never alter submitted keys.

Runtime-defined choices are a different problem. If the options exist as
values—conceptually `[(Text, x)]`—they cannot be recovered from `Generic`
without an input value. They are explicitly outside `askUser @T` v1 and may
later use a separate value-taking escape hatch. Do not contaminate the zero-
argument generic path with that concern.

## Canonical structural wire shape

The Haskell implementation uses a custom schema and decoder. The Rust
boundary may serialize those structures as JSON because the effect transport
is JSON-shaped; that serialization is an internal wire representation, not
an aeson contract the agent authors or derives.

Conceptually:

```haskell
data FormShape
  = StringShape
  | IntShape
  | NumberShape
  | BoolShape
  | OptionalShape FormShape
  | ProductShape TypeKey ConstructorKey [FieldShape]
  | SumShape TypeKey [VariantShape]
  | UnitShape

data FieldShape = FieldShape FieldKey FormShape
data VariantShape = VariantShape ConstructorKey FormShape
```

A matching answer algebra is:

```haskell
data FormAnswer
  = StringAnswer Text
  | IntAnswer Int
  | NumberAnswer Double
  | BoolAnswer Bool
  | OptionalAnswer (Maybe FormAnswer)
  | ProductAnswer [(FieldKey, FormAnswer)]
  | SumAnswer ConstructorKey FormAnswer
  | UnitAnswer
```

Names are illustrative. The production representation should reuse the
existing JIT-safe `Value` substrate where that reduces bridge work, but
schema production and decoding belong to one custom generic codec and must
not route through `FromJSON`.

Representative wire examples:

```json
{ "service": "api", "replicas": 2, "releaseNote": null }
```

for a record product, and:

```json
{
  "constructor": "Ssh",
  "fields": { "host": "example.com", "port": 22 }
}
```

for a payload-bearing sum. An all-nullary enum may use its constructor key as
a bare string. The exact compact encoding is frozen during the feasibility
spike; it must be unambiguous, recursively compositional, and keyed by the
same metadata used to produce the schema.

## Haskell API and implementation shape

The public API is:

```haskell
askUser :: forall a. GenericForm a => M a
```

`GenericForm` is an illustrative internal/facade constraint. It must be
automatically discharged from `Generic a` plus supported generic structure;
users neither derive it nor write instances for ordinary ADTs. User-facing
diagnostics and docs say only “derive `Generic`.”

An implementation may instead expose the literal internal constraint:

```haskell
askUser
  :: forall a
   . (Generic a, GFormCodec (Rep a))
  => M a
```

The generic codec owns both operations:

```haskell
gFormShape  :: proxy f -> FormShape
gFormDecode :: FormAnswer -> Either FormError (f p)
```

`to` converts the successfully rebuilt representation to `a`. This is the
load-bearing simplification: schema and decoder recurse over the same generic
structure and cannot disagree about field order, constructor tags, or nesting.

Internally:

- `M1 D` supplies datatype metadata;
- `:+:` supplies alternatives while preserving declaration order;
- `M1 C` supplies constructor metadata and a branch boundary;
- `:*:` supplies products;
- `M1 S` supplies selector metadata;
- `K1` dispatches recursively to a core leaf/container instance or another
  user-defined `Generic` type;
- `U1` supplies an empty constructor payload.

The implementation needs a visited-type set (or an equally strong mechanism)
so recursive types fail finitely rather than building an infinite schema or
dictionary loop.

## Compile-time error UX

Compile errors are part of the API. Golden tests match the useful lines while
allowing surrounding GHC wording to vary.

> **Amendment (2026-08-08, accepted at root; verified against the real
> extractor.)** Two of the messages below are not buildable as written, and
> the reason is a property of GHC rather than of our implementation.
>
> Whether a type HAS an instance is not observable from inside the type
> language. With `Generic` missing, `Rep a` is STUCK — indistinguishable
> from any other unreduced family application, and not apart from
> `M1 D d f` — so no fall-through equation fires and no `TypeError` can be
> raised. Branching on constraint satisfiability would require a
> typechecker plugin, out of scope by any reading.
>
> This affects **missing `Generic`** and **unsupported leaf** (whose
> rejection runs through the same mechanism, and so cannot list the
> supported leaves either). What ships instead carries the FIELD name inside
> the unsolved constraint, where it appears verbatim beside GHC's own
> `No instance for (Generic Environment)`. Field and nested type are both
> named — the property this section actually exists to guarantee — and only
> the prescriptive wording is lost.
>
> Every other message below is achievable and shipped, because those cases
> dispatch on a type that DOES reduce.

### Missing `Generic`

```text
`askUser @DeployRequest` cannot derive a form for DeployRequest.
Add: deriving (Generic)
```

For a nested custom type, the error names both the field and nested type:

```text
Cannot derive the field `environment :: Environment`.
Add `deriving (Generic)` to Environment.
```

### Unsupported leaf

```text
Cannot derive the field `deadline :: UTCTime`.
Supported leaves are Text, Int, Double, and Bool.
Define a small Generic ADT around supported leaves, or add a core FormLeaf instance.
```

Special-case common mistakes:

```text
`name :: String` is not supported; use Text.
`tags :: [Text]` needs a repeated-field editor; lists are not supported in v1.
```

### Recursive type

```text
Cannot derive a finite form for recursive field `children :: [Tree]`.
Recursive and repeated forms are not supported in v1.
```

### Ambiguous optionality

```text
`note :: Maybe (Maybe Text)` has nested optionality.
Use one Maybe layer or define an explicit sum with domain-named constructors.
```

Useful diagnostics must not lead with internal names such as `GFormCodec`,
`M1`, `K1`, or an overlapping-instance dump. Use `TypeError`, metadata carried
from `M1 S`/`M1 C`, and closed type-level dispatch where necessary. Standard
“No instance for Generic T” output is not sufficient for nested fields.

## Runtime behavior

1. `askUser @a` derives `FormShape` from type metadata without an `a` value.
2. It serializes the shape into the existing `AskUserWith` suspension.
3. The existing `OperatorGate` presents the shape and blocks for submission.
4. The same generic codec validates the recursive answer and rebuilds
   `Rep a`.
5. `to` yields `a`, and the continuation resumes with that typed value.
6. A malformed or incomplete answer re-presents the same form through the
   existing bounded retry path.

No `FromJSON`, `ToJSON`, `Read`, partial constructor lookup, or source-code
generation participates in the answer path. No new effect constructor,
routing arm, or driver capability is introduced.

## Migration from applicative `Form`

This:

```haskell
Reply <$> enumField "Lane" [("alpha", Alpha), ("beta", Beta)]
      <*> intField "Count"
```

becomes:

```haskell
data Lane = Alpha | Beta deriving (Generic)
data Reply = Reply { lane :: Lane, count :: Int } deriving (Generic)

askUser @Reply
```

Migration requirements:

1. Replace the builder-taking `askUser :: Form a -> M a` with the
   type-directed `askUser @a`.
2. Remove `Form`, `enumField`, `intField`, `textField`, and `boolField` from
   the auto-imported and advertised surface.
3. Migrate repository fixtures and examples to ordinary `Generic` ADTs.
4. The old builder may temporarily live in an explicitly internal/legacy
   module to keep the change bisectable, but it is not a permanent second API.
5. Delete positional `f<n>` field generation; positions are used only inside
   positional product nodes, never as a global form-wide counter.

## Acceptance criteria

### Surface

- A fresh resident session can declare the example ADTs and run
  `askUser @DeployRequest` without imports, pragmas, codecs, form builders, or
  Tidepool-specific instances.
- The agent-facing description fits in one paragraph and one example.
- The resulting binding has exactly the requested Haskell type.

### Structural coverage

- Round-trip tests cover every primitive leaf.
- Round-trip tests cover named records, positional products, newtypes, and
  tuples.
- Nullary sums of 2, 3, and 5 constructors preserve source order and round
  trip every constructor.
- Payload-bearing sums round trip nullary, positional, and record branches.
- Nested product-of-sum, sum-of-product, and sum-of-sum examples round trip.
- `Maybe` round trips both absence and presence around a leaf, product, and
  sum.
- `False`, zero, empty `Text`, nullary payloads, and identical leaf values in
  different branches remain distinguishable.
- Selector and constructor keys remain exact while display humanization
  changes labels only.

### Rejection and diagnostics

- Compile-fail fixtures cover missing `Generic`, `String`, lists, maps,
  functions, recursive ADTs, and nested `Maybe`.
- Every failure names the nearest source-level field/constructor/type.
  Prescriptive correction text accompanies it wherever the offending type is
  observable — `String`, lists, maps, functions, nested `Maybe`, recursion.
  It is NOT achievable for missing `Generic` or an unsupported leaf; see the
  amendment under "Compile-time error UX".
- No useful error requires understanding a generic representation type.
- Malformed wire answers, unknown keys, duplicate keys, unknown constructor
  tags, missing product fields, and extra product fields are rejected without
  consuming the continuation.

### Integration

- Existing `AskUser` routing and both headless and web `OperatorGate`
  implementations continue to work.
- A real-extract/JIT acceptance test declares nested ADTs in a resident
  session, suspends on `askUser @T`, submits through a test gate, and observes
  the typed value after resume.
- The web UI renders primitive leaves, enums, optional groups, and
  payload-bearing sum branches from the derived structure.
- The bounded malformed-submission re-prompt test remains green.

## Delivery sequence

1. **Generic codec spike.** Prove the exact `deriving (Generic)` plus
   `askUser @T` spelling through the real extractor/JIT. Round trip a nested
   sum/product and prove one selector-aware `TypeError`. Freeze the recursive
   answer encoding here.
2. **Core algebra.** Implement `FormShape`, custom generic decode, primitive
   leaves, `Maybe`, `()`, and pure round-trip tests.
3. **Diagnostics.** Add visited-type tracking and source-level `TypeError`
   dispatch before widening shape coverage.
4. **Wire and renderer.** Teach the shared Rust spec types and web renderer
   recursive products/sums; validate exact keys.
5. **`askUser` integration.** Replace the builder-taking API while preserving
   the effect and retry machinery.
6. **Migration and deletion.** Convert fixtures/docs, remove builder
   advertising, then delete the old implementation once acceptance is green.

## Future extensions

- `[a]` and `NonEmpty a` after a repeatable-control and finite answer contract
  exists.
- `Day`, `UTCTime`, file references, secrets, and multiline text as blessed
  semantic leaves.
- Runtime option sets through a separate value-taking API, plausibly keyed as
  `[(Text, a)]`; this solves a genuinely different problem from type-defined
  sums.
- Defaults, ranges, descriptions, and presentation hints through a small
  optional annotation layer.
- User-defined leaf instances only if real tasks need semantic scalars the
  core cannot reasonably bless.

Extensions must preserve the zero-metadata generic path. An annotation layer
may refine rendering or validation; it must never become mandatory tuple
construction beside the type.

## Decision record

- **Chosen:** `askUser @T` over a custom bidirectional `Generic` traversal.
- **Chosen:** constructor and selector metadata as canonical structural keys.
- **Chosen:** arbitrary finite sums/products over a few blessed leaves.
- **Chosen:** special semantic implementations for `Bool`, `Maybe`, and `()`.
- **Rejected:** `FromJSON`/`ToJSON`; they add an unrelated encoding contract,
  duplicate generic traversal, and weaken control of diagnostics.
- **Rejected:** a Tidepool-specific deriving class; `Generic` is sufficient
  and already in model latent space.
- **Rejected:** Yesod/`optparse-applicative`/the current applicative `Form`;
  all redundantly reconstruct a shape the ADT already contains.
- **Rejected:** agent-authored HTML or JSON Schema; useful wire languages, but
  unnecessary authoring surfaces here.
- **Deferred:** `[(Text, a)]` runtime choices; they require a value-taking API
  and should not complicate type-defined sums.
- **Amended (2026-08-08):** prescriptive correction text for missing
  `Generic` and for an unsupported leaf is unbuildable without observing
  constraint satisfiability, which the type language cannot do — a stuck
  `Rep a` is indistinguishable from any other unreduced family application.
  Verified against the real extractor. The acceptance criterion is now that
  a failure NAMES the nearest field/constructor/type; the correction text
  stands wherever the offending type reduces. Reversing this needs a
  typechecker plugin, not a cleverer type family — do not re-attempt it as
  written.

