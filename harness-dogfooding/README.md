# harness-dogfooding

Live authored harnesses for the self-iterating harness system — **Xmonad.hs
vibes**: you write `render` / `loop` / `State` in plain Haskell, the runtime
(`tidepool-selfharness`) drives it. This is *our* dogfood space (distinct from
`examples/harness/`, which holds the frozen reference contract), where the
"smart agent" (a clean-context Claude session) authors and iterates the harness
by hand — the manual precursor to autonomous distillation.

Each subdirectory is one authored harness, following the two-file contract:

- `Harness.hs` — the config (Xmonad-like): re-exports the vocabulary + `render`
  and defines `loop` (the one effectful piece; uses `runLLMTurn`, and — once it
  lands — `askUser` for direct operator elicitation).
- `HarnessTypes.hs` — `State` + the answer ADTs + the pure `render`, with **no**
  reference to `runLLMTurn`/`loop`, so the nested answerer can import the answer
  types without pulling in the outer effect row (the harness/agent split).

Point the driver at a subdir's `Harness.hs`; its directory becomes the include
root so the sibling `HarnessTypes` resolves.

**Minimal effect surface, on purpose:** the agent inside has only `askUser`
(typed forms: enum / int / text / bool) + `finalize`, plus `fork` as it lands.
No fs / exec / http — we prove the loop on legible flows before adding capability
breadth. Frictions hit while authoring/running these *are the roadmap* for the
next harness helpers.

## Harnesses

- [`wizard/`](wizard/Harness.hs) — **feature-brainstorm / PRD-construction
  thought-partner**, pointed at tidepool itself. A frame → diverge → converge →
  draft wizard: the agent supplies per-step cognition, the operator supplies
  taste through forms, and the accumulating `draft` *is* a stream of change
  requests. Deliberately rough — the roughness is iteration fuel.
