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

**Minimal effect surfaces, on purpose:** the wizard's answerer has only
`askUser` (typed forms) + `finalize`, plus `fork` as it lands. The development
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
  recursive `DevPlan` into coding agents, pokes descendants when parent HEADs
  move, then asks fresh integration agents to merge completed branches
  bottom-up. It is intentionally tagged against PRD 18 and the forthcoming
  narrow Worktree/Event PRD and will compile as those surfaces land.
