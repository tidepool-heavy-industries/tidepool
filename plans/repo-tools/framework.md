# framework.md — the typed-tool core

The pure-Haskell type machinery. Lives in `.tidepool/mcp/lib/Tool.hs`, loadable by
tool modules and eval-testable in isolation.

## Motivation

Because tools are durable code, the boundary keeps its types. `run :: Args -> M
Result` over real records and sum types buys what an ad-hoc eval throws away:

1. **Compiler-caught contract drift** — rename a field and every caller/test breaks
   at compile time.
2. **Derived schemas, both directions** — input schema (for `tools/list` + parsing)
   and output schema, from the types.
3. **Typed tests** — `run (Args …)` compared to a `Result`, not string-diffing JSON.

This is the thing ad-hoc evals can never give: a typed, versioned, testable API
surface.

## Author surface

```haskell
data Args   = Args   { dirs :: [Text], kinds :: Maybe [Text] } deriving Generic
data Result = Result { hits :: [Text], skipped :: Int }        deriving (Generic, ToJSON)
run :: Args -> M Result
tool = mkToolAuto "gotcha-scan" "scan dirs for known gotchas" run
```

Three tiers:
- **`mkToolAuto name desc run`** — schema fully derived from `Args` (records + sums),
  output schema from `Result`. Zero schema authoring.
- **`mkTool name schema run`** — author an input-schema `Value` (`[j|…|]`) when you
  want per-field descriptions / tightening; `conformsTo (schemaOf @Args) schema` is
  checked at boot, type-aware.
- **raw escape hatch** — `run :: Value -> M Value` for genuinely dynamic args
  (untyped; nothing to conform-check). The "intermediate / leave it available" tier.

## Machinery (all generic, all JIT-proven)

- **`McpSchema` and `LlmSchema`** (two classes, one shared generic `Rep` walk) —
  derive a JSON Schema from a type. They split because the targets diverge:
  **`McpSchema`** (tool I/O) renders `Maybe`→not-required and sums→`oneOf` (aeson
  `TaggedObject`: `{"tag":"Foo", …fields}`); **`LlmSchema`** (structured-output)
  renders `Maybe`→*nullable-but-present* and prefers string `enum` for nullary sums
  (strict-mode rules — see Open questions). Two classes, not a mode flag, so the
  boundary is in the type: a tool arg derives `McpSchema`, an LLM result derives
  `LlmSchema`, and you can't render a type for the wrong boundary.
- **`FromValue` / `GFromV`** — hand-rolled generic parser `Value → record`. Needed
  because vendored aeson's generic `FromJSON` was stripped (PR #144); we route around
  it. ~25 lines, proven. It is the **one shared parser** for every boundary (JSON is
  JSON) — but it must treat a `Maybe` field as `Nothing` on *both* absent (MCP) and
  explicit `null` (LLM strict-mode), so one parser serves both schema renderers.
- **Output** — `ToJSON` (derived) for the result + a derived output schema.
- **`conformsTo :: Value -> Value -> Bool`** — boot check: the `ToSchema`-derived
  structure must be a sub-schema of the authored schema (which may add descriptions
  / tightening, may not contradict). Type-aware — a schema that mistypes `dirs` as
  string, or marks `kinds` required, is rejected at boot, by name. Pure lib, so the
  strictness *policy* is programmable Haskell, eval-testable, not frozen in Rust.
- **`ToolDef` / `Server`** — the record-of-functions; one aggregate module, one
  compile (see [server.md](server.md)).

```haskell
data ToolDef = ToolDef { tName :: Text, tSchema :: Value, tOutSchema :: Maybe Value, tRun :: Value -> M Value }
data Server  = Server  { sDesc :: Text, sTools :: [ToolDef] }
mkTool     :: (Generic a, McpSchema a, FromValue a, Generic r, McpSchema r, ToJSON r) => Text -> Value -> (a -> M r) -> ToolDef
mkToolAuto :: (Generic a, McpSchema a, FromValue a, Generic r, McpSchema r, ToJSON r) => Text -> Text  -> (a -> M r) -> ToolDef
conformsTo :: Value -> Value -> Bool
```

## The typed LLM/ask surface

The framework already derives a schema from a type and parses a `Value` back into it
— so the *LLM boundary inside a tool* gets the same treatment, no `Value` cruft, no
hand-built `Schema` ADT. The schema comes from **`LlmSchema`** (the structured-output
renderer); the response is parsed by the shared **`FromValue`**:

```haskell
data Decision  = Expand | Prune | Escalate
  deriving (Generic, LlmSchema, FromValue)
data NodeClass = NodeClass { decision :: Decision, reason :: Maybe Text }
  deriving (Generic, LlmSchema, FromValue)

llmAuto :: (Generic a, LlmSchema a, FromValue a) => Text -> M a   -- server-side LLM
askAuto :: (Generic a, LlmSchema a, FromValue a) => Text -> M a   -- suspend to the calling agent

classify :: Symbol -> M NodeClass
classify sym = llmAuto (nodePrompt sym)   -- schema derived; response parsed; can't return a 4th option
```

So the structured boundaries share **one parser (`FromValue`) and two schema
renderers** — `McpSchema` for tool I/O, `LlmSchema` for the LLM — split because their
targets have different rules (see Open questions). Not four ad-hoc JSON dances, but
not one class either: the boundary you render for is in the type. The `Schema` ADT +
`Value`/optics survives only as the dynamic escape hatch. The author thinks in types:
the prompt is a typed builder, the response is a real value to `case` on,
`classify`'s prompt is unit-testable in isolation, and the model behind it swaps
without touching callers. This is the durable-code payoff aimed at the LLM call
itself.

## Feasibility (proven)

Spiked live through the JIT on `data Args = Args { dirs :: [Text], kinds :: Maybe
[Text] } deriving Generic`:

| mechanism | result |
|---|---|
| `Rep` reflection (`selName`) — field names | `["dirs","kinds"]` |
| `Maybe`-optionality (overlapping `S1 c (Rec0 (Maybe a))`) | `[("dirs",true),("kinds",false)]` |
| `ToSchema` type→JSON, incl. nested `[Text]` | `{"dirs":{type:array,items:{type:string}},…}` |
| generic parser `Value→Args` (`to`-direction, builds `M1`/`K1`/`:*:`) | `Args {dirs=["src","lib"], kinds=Nothing}` |

Only vendored aeson's *built-in* generic `FromJSON` fails (parser stripped) — the
hand-rolled parser covers it. Sum-type `oneOf` and output-direction schema extend
these *same proven walks*; no new risk class.

## Decisions + rationale

- **Sum types → `TaggedObject`** (tag field `"tag"`). Default `ToJSON` is free, and
  our derived `ToSchema` mirrors the default shape, so runtime JSON and advertised
  output-schema agree *by construction* — `conformsTo` holds with no hand-maintained
  custom `ToJSON`/`ToSchema` pair. Prefer record constructors (named fields carry
  descriptions); positional constructors fall under `"contents"`.
- **Schema authored in-Haskell + checked — not derived-only.** Per-field
  descriptions can't come from a type without TH `getDoc` (needs `-haddock`
  plumbing; rejected — see the journey). So the author writes the schema `Value`
  (with prose) and `ToSchema`+`conformsTo` *check* it against the record;
  `mkToolAuto` covers the no-prose case by deriving outright.

## Open questions

- **`conformsTo` exact relation** — derived ⊑ authored on `{properties (names+types),
  required}`; authored may add `description`/`enum`/bounds. Pin the precise rule
  (it's pure lib, eval-testable).
- **Recursive / parametric types** — recursive ADTs → JSON Schema `$ref`/`$defs`, or
  forbidden in tool I/O for v0? Most args are flat; flag the boundary.
- **Non-record sum constructors** — the `"contents"` wrapping is awkward for LLM
  I/O; lint toward record constructors, or support positional cleanly?
- **Generic-derivation cost at scale** — the spikes are tiny; does `ToSchema`/parse
  through the JIT stay cheap for large/nested types? Measure.
- **`McpSchema` vs `LlmSchema` — exact rules.** The split is decided (two classes,
  shared `Rep` walk). Pin each renderer's precise divergence: `Maybe`→omit (MCP) vs
  nullable-present (LLM); sums→`oneOf` (MCP) vs string-`enum`-preferred (LLM, since
  payload-carrying `oneOf` is unevenly supported by strict mode);
  `additionalProperties:false` for LLM strict. Confirm the shared `FromValue` treats
  absent *and* `null` as `Nothing`. Steer LLM-*result* types toward enums + flat
  records. (Whether the two classes share one policy-parameterized walk is an impl
  detail.)
