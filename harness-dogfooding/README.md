# harness-dogfooding

Live authored harnesses for the self-iterating harness system — **Xmonad.hs
vibes**: you write `render` / `loop` / `State` in plain Haskell, the runtime
(`tidepool-selfharness`) drives it. This is *our* dogfood space (distinct from
`examples/harness/`, which holds the frozen reference contract), where the
"smart agent" (a clean-context Claude session) authors and iterates the harness
by hand — the manual precursor to autonomous distillation.

Each subdirectory is one authored harness. The current examples follow the
two-file layout:

- `Harness.hs` — the config (Xmonad-like): re-exports the vocabulary + `render`
  and defines `loop`, the one effectful piece. A loop may ask a resident model
  for cognition, orchestrate typed subagents directly, or combine both.
- `HarnessTypes.hs` — `State` + the answer ADTs + the pure `render`, with **no**
  reference to `loop` or runtime handles, so answerers and checkpoint codecs can
  import durable vocabulary without pulling in the outer effect row.

Point the driver at a subdir's `Harness.hs`; its directory becomes the include
root so the sibling `HarnessTypes` resolves.

**Minimal effect surfaces, on purpose:** the wizard's answerer row is exactly
`Eff '[AskUser, Fork, Finalize]` (spelled out in `Tidepool.Agent`) — typed
forms, depth-one parallel sub-answerers, and the typed yield. The development
tree instead gives headless workers their native coding tools while Haskell
owns typed orchestration. Frictions hit while authoring/running these *are the
roadmap* for the next harness helpers.

## Harnesses

- [`wizard/`](wizard/Harness.hs) — **feature-brainstorm / PRD-construction
  thought-partner**, pointed at tidepool itself. A frame → diverge → converge →
  draft wizard: the agent supplies per-step cognition, the operator supplies
  taste through forms, and the accumulating `draft` *is* a stream of change
  requests. Deliberately rough — the roughness is iteration fuel.
- [`dev-tree/`](dev-tree/Harness.hs) — **forward dogfood for typed subagents,
  retained worktrees, and lexical event handlers**. It unfolds an ordinary
  recursive `DevPlan` depth-first into coding agents — parent worker first, so
  each child worktree is seeded from a parent HEAD that is already final, which
  is why no rebase propagation is needed — then asks fresh integration agents
  to merge completed branches bottom-up. It is written against PRD 18's typed
  `spawnAgent @r` (lane 1: synchronous, one cycle) and PRD 19's managed
  worktrees + events, both of which have landed. What it waits on is ROW
  COMPOSITION, not API: `Harness` is an alias for `M`, and the driver's v1
  outer session is `RunLLMTurn`-only, so this file needs
  `Console`/`Worktree`/`RepoEvent`/`Subagent` appended to that row before the
  driver can run it. `tidepool-harness/tests/dogfood_harness_typecheck.rs`
  compiles it against that row.
