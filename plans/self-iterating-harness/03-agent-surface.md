# Agent surface — typed yield, finalize, effects, fork

## The typed yield

```haskell
runLLMTurnWithRequiredResp :: ... -> Harness a    -- proposed shape
runLLMTurn                 :: ... -> Harness ()    -- no required response
```

- The required response type `a` is **monomorphic**, **annotate-or-infer**
  (`readLn`-shaped): inferred from usage when usage constrains it, annotated
  (`@A`) otherwise. It must be **concrete at the call site** — the LLM needs a
  specific shape to produce and the handler compiles the response against it;
  you cannot ask for "an `a` for all `a`". LOCKED (monomorphic; polymorphic
  yields are a research fork, §06).
- Builds directly on `harness-r0`'s `returnControl @T`: **GHC is the schema
  validator.** An ill-typed answer does not consume the continuation; the GHC
  error becomes the retry prompt.

## Parked continuation + retries

`runLLMTurn` parks the `Harness` continuation `k :: a -> Harness r`. The agent's
turn fills the hole; the `Harness` layer performs the type-retries. Higher-level
combinators (e.g. a multi-turn `runAgentUntil`) are **library code** built on
these primitives, not part of the core — a coding LLM can write them.

## One agent tool + `finalize`

The inner agent has a **single interface**: run a repl statement in the session.
It runs statements freely until ready.

Finalization — producing the required response and returning control to
`Harness` — is an **effect**, not a second tool:

```haskell
finalize :: a -> Agent x     -- proposed shape; name LOCKED as `finalize`
```

`finalize` carries the typed value back to `Harness` **in-heap**, so the value
may be nonserializable (a function, closure, or effectful action). This is why
"finalizing" is one effect and not a separate finalizing-tool: it keeps the
agent surface to a single tool.

## Agent effect set

- **Goal:** the **harness code determines the per-context effect set** — narrow,
  typed vocabularies that vary per turn/stage (the capability surface is chosen
  by the harness, not fixed).
- **v1:** a **hardcoded maximal stack, identical in all contexts**, while we
  iterate on the DSL. Start with whatever `tidepool-harness` already provides —
  gui (the `Ui` eDSL) and `fork`. Add "actually-doing-things" effects (file
  edits, git, exec, http, …) **later** (§06).

## `fork` — branching / the in-language hylo

`fork` is a `Harness` effect. It reuses `harness-r0`'s fork machinery to split
the agent session into multiple **logical threads** off a single context window
— each thread (logical, not OS) independently appending user/tool/etc turns as
it runs. It forks `Harness` computations; the underlying agent session forks
with it.

This is subagent branching, the tree version of the render/loop hylo (§01), and
the in-language analogue of exo's `fork_wave`/`merge`.

## "Typed stages"

In v1, a "stage" is **just a sequenced `runLLMTurnWithRequiredResp @T` call** in
ordinary Haskell control flow inside `loop`. There is **no first-class `Stage`
type and no type-level OODA/stage DSL** (§06). Branching between stages is plain
Haskell `case`/`if`. OODA is one example shape `loop` might take, not a baked-in
structure.
