# Bug: eager App-argument evaluation forces GHC's absent-demand bottoming fallbacks

**Status:** RESOLVED — fixed in `bccfef9` (2026-06-21, "fix(codegen): lazy poison
for error calls in App-argument position"). Both halves landed:
- **A** (`emit/expr.rs`): `EmitFrame::RaiseLazy` + `collect_app_arg_positions` —
  an error call in App-argument position routes to a lazy poison closure.
- **C** (`Translate.hs:1914`): `lastError`/`initError` added to `isErrorVar`.
Regression tests GREEN: `works_lens_last_init_unsnoc_on_list` +
`works_lens_last_on_text` (tidepool-runtime `gotcha_registry`). The body below is
retained as root-cause history; the "Proposed fix" was implemented as A+C.

> NOTE: live `^? _last` needs an explicit `import Control.Lens (_last, …)` — those
> optics are not in the default preamble. That's an import requirement, not the bug.
**Symptom (confirmed live):** `[10,20,30::Int] ^? _last` (Control.Lens, list) fails with
`haskell-error: yield error: Haskell error: last`. Same for `_init`/`unsnoc` on lists.

## What is and isn't affected
- `_head` on a list → **works**. Only `_last`/`_init`/`unsnoc` (the back-of-list optics) fail.
- Failure is for **every list provenance** (`[10,20,30]`, `[1..3]`, `map (+1) …`, `['a','b','c']`)
  — element type is a **red herring**.
- `"abc" ^? _last` → **works**: `OverloadedStrings` makes it `Text`, routed through the
  **vendored `Snoc Text`** instance (no error worker). So it's a **list-vs-Text instance**
  difference, not the optic.
- Manual `if null xs then … else (init xs, last xs)`, and direct `P.last`/`GHC.List.last`
  → **work**. So `last` itself is fine.

## Mechanism (confirmed via TIDEPOOL_DUMP_CLOSED Core dump)
`[10,20,30] ^? _last` compiles (with `-O2`, `Opt_CrossModuleSpecialise` ON) to:

```
probeLast = $sgo1 (I# 10) [20,30] (lastError @Int (PushCallStack "last" …))
$sgo1 = \head tail _ -> go1 tail head     -- 3rd arg [Occ=Dead, Dmd=A]  (ABSENT)
go1   = \ds eta -> case ds of { [] -> eta; (y:ys) -> go1 ys y }
```

GHC's `INLINE _Snoc` + cross-module specialization of `GHC.List.last`; **demand analysis
proves the fallback arg Absent** (the list is statically known non-empty here), so passing the
bottoming `lastError "last"` thunk in that slot is lazy-safe **in GHC**. But the **JIT evaluates
App arguments EAGERLY**, so it forces the dead `lastError "last"` thunk → raises "last".

**Bug class:** eager App-argument evaluation × GHC absent-demand **dead arg slots holding a
bottoming fallback** (`error`/`lastError`/`initError`). Not lens-specific — any
worker/wrapper specialization that hoists a bottoming fallback into an `[Occ=Dead, Dmd=A]`
argument trips it.

## Why the existing error-sentinel machinery (#11) misses it
1. `lastError` is not in `isErrorVar` (Translate.hs:1893 lists `errorEmptyList`, not its wrapper
   `lastError`) → the Var is untagged.
2. Deeper: **even if tagged**, an error-call in **argument position with a static message** takes
   the eager-`Raise` short-circuit (`expr.rs:176-185`), and `EmitFrame::Raise` collapses to an
   **eager** `runtime_error_with_msg` (`expr.rs:802-854`). The lazy poison-closure path that would
   correctly defer is only used for the no-static-message partial-app case. #11's poison defers
   `let`-RHS error bindings, not inline error exprs sitting in dead arg slots.

## Proposed fix (ranked)
- **A (most robust; principled — matches Haskell's non-strict `error`):** error-calls in
  **App-argument position** must emit a **LAZY poison closure**, never an eager `Raise`. Forced
  positions (scrutinee / RHS / tail) still raise by forcing the poison. Requires the emit descend
  to know "I'm an App arg" (mirror how non-trivial `Con` fields become `ThunkCon`).
- **B (narrower):** always use the lazy poison path for error-call Apps (drop the static-msg
  eager-`Raise` short-circuit entirely). Simpler; slightly less precise messages (poison
  trampoline materializes the message on force).
- **C (insufficient alone):** add `lastError`/`initError` to `isErrorVar` — still eager-raises in
  arg position. Only useful combined with A/B.

## Risk + gate
Touches App-argument evaluation strategy in the **emit hylomorphism** — the area memory flags as
**#313-class conversion risk**. Tractable but NOT obviously low-risk. Gate any fix on the **full
workspace suite + the JIT-vs-eval differential proptests** green.

## Repro / acceptance
- `[10,20,30] ^? _last` → `Just 30`; `^? _head` → `Just 10`; `_init`/`unsnoc` on lists correct.
- `"abc" ^? _last` stays working (Text path).
- Add a regression test alongside the eval/suite tests.
