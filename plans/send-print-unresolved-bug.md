# RESOLVED: `send (effect) >> pure` → unresolved `GHC.Magic.nospec`

Found + fixed 2026-06-20 via dogfooding. Regression guard:
`tidepool-mcp/tests/repro_print_unresolved.rs` (4/4 green).

## Symptom (was)
A unit-returning effect followed by a `pure` continuation failed at runtime with
`unresolved variable VarId(0xfe75fa6b4241aaa3)`; `run >> pure` (tuple result)
worked. Broke `say`/Console on the dogfooding server.

## Root cause (confirmed)
The var `0xfe75fa6b4241aaa3` is **`GHC.Magic.nospec`** — the specializer's
identity wrapper (`nospec x = x`), which GHC inserts to block over-specialization
of dictionary / class-method code. It has **no unfolding**, so `resolveExternals`
can't inline it; the extract emitted it as a plain `NVar` (it believed it
resolved), and the JIT — which only special-cased `runRW#` among the magic
functions — had no binding/con for it → forced it as an unresolved external.
`run >> pure` differs in its dictionary path, so `nospec` isn't inserted there.

It is a **regression from re-enabling `Opt_Specialise`** (GhcPipeline.hs): once
specialization is on, GHC emits `nospec`. (The earlier "a9a0082 pruned ()"
diagnosis was wrong — disproved by reproducing `stableVarId`: `()` hashes to
`0xfeb080…`, not `0xfe75…`; the `0xFE` byte is the universal prefix, not a tag.)

## Fix
Treat `GHC.Magic.nospec` as the identity in `Translate.hs` (like `runRW#`):
- App form: `nospec @t f x… → f x…` (drop the wrapper, apply its first value
  arg to the rest).
- Bare/zero-value-arg form (`translateHead`): emit the identity `\x -> x`.
- New predicate `isNospecVar` (occ `nospec` + module `GHC.Magic` after
  normalizeMod).
Validated: repro_print_unresolved 4/4; haskell_suite 217; and the LIVE server now
runs `send (Print x) >> pure y` (and bare `send (Print …)`) correctly — `say`
restored.

## Follow-up (separate, NOT a regression)
`eval_partial_output_failclass::say_then_stack_overflow` is re-`#[ignore]`d. Its
DEEP-recursion variant (`>> (pure $! (go 5_000_000))`) trips a spurious
`[CASE TRAP]` in the test's MINIMAL-stack, signal-handler-less worker harness —
but the **live full-stack eval of the exact same expression yields cleanly**
(runtime-yield, verified). So it's a harness-fidelity gap (install signal
handlers / use the full effect stack), not a codegen regression. The nospec
cases it cared about are covered by repro_print_unresolved.
