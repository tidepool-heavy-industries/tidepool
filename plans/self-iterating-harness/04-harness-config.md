# The harness configuration — pure Haskell, git-versioned

## What a harness *is*

A harness is **pure Haskell** and nothing else:

- the `render :: State -> Maybe Text -> Text` function,
- the `loop :: State -> Harness State` function,
- the `State` type (with its `ToJSON`/`FromJSON` instances),
- any helper functions,
- prompt text via `[fmt|]`.

No template language, no config format, no separate DSL file. The "DSL" is
Haskell in the two functions. Conditional context is Haskell conditionals over
`State`.

## Versioning

- The harness lives in a **git repo**. The runtime loads a given commit/head per
  session; a session is **pinned to a version**.
- **Local git for v1** — no GitHub needed. (Future, §06: "each harness is a
  GitHub repo", leaning on agent competency at PR/iteration flow — a nice
  possibility, explicitly deferred.)
- Picking up a new version is by **runtime restart** (§02). State is serialized
  across the restart.

## What the DSL earns

The value of expressing the harness as this constrained Haskell surface is what
it makes **unrepresentable**, not what it enables:

- no per-turn system-prompt re-render / cache-blow (`render` is runtime-invoked
  at boundaries only),
- no untyped yield (`@A` pins the response type; GHC validates).

Servant-style type-level machinery to make bad wiring fail to compile was
discussed but is **not in v1** — revisit only if expressing the wiring actually
needs it (§06). Likewise there is no type-level stage/OODA graph in v1.

## Migration

If a new harness version changes the `State` format, basic **migration between
versions** is required (State is JSON-serialized across loops/restarts). The
mechanism is deferred (§06); the requirement is noted so a format change is not
assumed free.
