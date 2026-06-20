# Bug: unit-returning effect `>> pure` → unresolved `()` variable

Found + root-caused 2026-06-20 via dogfooding. **Regression from commit
`a9a0082` (table-hygiene: "scope DataConTable meta walks to reachable
closure"), recent.** Reproduced in tests: `tidepool-mcp/tests/repro_print_unresolved.rs`.

## Symptom
A unit-returning effect followed by a `pure` continuation fails at runtime:

```
yield error: unresolved variable VarId(0xfe75fa6b4241aaa3) [tag='þ', key=…]
```

(deterministic VarId = the `()` / unit constructor).

## Minimal trigger (mapped in repro_print_unresolved.rs)
- `pure 123` ✓
- `send (Print x) >> (error …)` ✓ — Print resolves (the `()`-tag-0 issue is
  masked: the error is thrown before the unit result must be produced)
- `send (Print x) >> (pure 123)` ✗ — unresolved `()`
- `send (KvSet k v) >> (pure 1)` ✗ — same (KvSet also returns `()`)
- `run "echo hi" >> pure 42` ✓ — `run` returns a tuple, which IS retained

So the class is **any unit-returning effect (`()` result) whose result is
discarded by a `pure` continuation**, not Console-specific. `run >> pure` works
because tuple constructors are retained by the meta walk.

## Root cause (confirmed)
`a9a0082` changed the DataConTable meta walks (`collectUsedDataCons` /
`collectTransitiveDCons`) to harvest constructors only from the target's
**reachable VALUE bindings** (`reachableBinds`) instead of the full closed bind
set — a correct fix for the 56-bit varId-collision flood (909→172 entries). But
its retained-con list (`I#/D#/C#/F#/W#`, list, tuples, Integer, Map) **omits
`()`**, because `()` is never *lexically constructed* in the user's Core for
`effect () >> pure y`: the `()` is injected by the effect HANDLER at runtime and
discarded by the `pure` continuation. Pre-`a9a0082`, the full-closed-set walk
swept `()` in incidentally, so it worked.

General statement of the gap: **constructors that effect handlers inject at
runtime (the effects' RESULT-type cons) must be in the table even when they are
not reachable from the user's value bindings.** `()` is the dominant case; a
discarded `Maybe`/`Bool`/etc. effect result would hit the same gap.

## Fix options (all in haskell/src/Tidepool/Translate.hs; need extract rebuild
##  + suite_cbor meta regen + retest; must NOT revert to the full-closure walk)
1. **Principled** — for each effect constructor used in the reachable binds,
   also include the constructors of its GADT RESULT type (`Print :: … ->
   Console ()` ⇒ include `()`; an effect returning `Maybe Value` ⇒ include
   `Just`/`Nothing`). True mechanism fix; needs GADT-result-type analysis.
2. **Pragmatic wired set** — seed the meta walk with a fixed set of
   runtime-injectable cons: `()` (+ `Just`/`Nothing`, `True`/`False`, and the
   `Value` cons). Covers all realistic effect results; simple; no GADT analysis;
   small risk of missing an exotic result type.
3. **Minimal** — seed just `()` (unitDataCon). Fixes exactly the reported bug;
   leaves rarer discarded non-unit effect results (e.g. a thrown-away `Maybe`).

Implementing any of these and watching the repro tests flip green is the
forward confirmation of the diagnosis (cheaper than rebuilding the old extract).

## Status
- Reproduced + root-caused. `repro_print_unresolved.rs` has the failing cases
  `#[ignore]`d (un-ignore when fixed → they become regression guards).
- `eval_partial_output_failclass::say_then_stack_overflow` also `#[ignore]`d
  (it hit this same bug via `>> (pure $! …)`).
- Live dogfooding impact: `say`/Console (and any unit-returning effect followed
  by `pure`) is broken until the fix ships.
