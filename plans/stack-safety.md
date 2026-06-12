# Stack-safety pass — fit-to-paradigm allocation

Status: ratified by human 2026-06-11; queued behind current P1s (union-fix, emit
frame-diet). Trigger incident: lit-tolerance fix fattened emit_data_dispatch's
frame (~20-40KB/level × 50-99 case-nesting levels ≈ 1.5-2MB on 2MB test threads
→ compile-time stack overflow, presented as committed:: proptest SIGSEGV).

Principle: use recursion-schemes where the algorithm IS a scheme; use stack
growth where it's an effectful interpreter wearing recursion as syntax. Frame
size is an invisible emergent property — explicit work-stack state is a visible,
testable struct (`assert!(size_of::<Frame>() <= 64)`).

## recursion-crate conversions (true schemes — high fit, low risk)

1. **tidepool-optimize passes** (beta, DCE, inline, case-reduce): pure
   CoreExpr→CoreExpr over RecursiveTree<CoreFrame> — textbook catamorphisms,
   zero effect-threading, walk the same deep trees that bit emit.
2. **Deep-Drop work-lists**: recursive Drop on Value spines kills host threads
   (host-stack-overflow class; presents as silent hang). Explicit drop queues.
3. **Serialization walks**: value_to_heap already proved the pattern in-tree.

## stacker at the emit spine (interpreter — keep the RAII)

`stacker::maybe_grow` at emit_node. Emit is NOT converted to a work stack:
- it is effectful (builder mutation) with threaded context — #313 (TailCtx
  leaking through the existing emit hylo) lives exactly at that seam;
- env save/restore is RAII-shaped; work-stack conversion turns every pair into
  two entries that must pair by construction, with SILENT MISCOMPILATION (not
  a crash) as the failure mode.
Payoff of conversion there is cost-model visibility only; risk is dominated by
#313-class bugs. stacker removes the cliff for ~5 lines and zero invariant risk.

## Measured (sigsegv-hunt, 2026-06-12)

Deterministic threshold method: spawn thread of size N, compile a fixed deep
program (`toUpper (strip ("" <> "o6s\nc1m"))`), binary-search smallest N that
compiles (4 KiB resolution):
- baseline 1cdbd68: ~2033 KiB — already at the 2048 KiB libtest cliff
  (the gate was green-by-entropy; proptest seeds are non-deterministic)
- lit-fix HEAD: ~2045 KiB (+12 KiB); frame-slim prototype recovered only ~4
- frame-slim was therefore REJECTED: cannot restore margin that never existed,
  and would churn validated emit code for an immaterial gain

Shipped instead (7c52329): proptest compiles route through a 64 MiB worker —
the SAME mechanism production uses (tidepool-mcp/src/lib.rs:2165 = 256 MiB
eval threads). All eval compilation must run on a large stack until the emit
work below lands.

## Sharpened direction

emit_node's Let-spine is ALREADY trampolined; the native recursion that costs
~2 MiB is CASE-ALT BODY emission (emit_node↔collapse_frame↔emit_case↔dispatch,
depth ~50-99 × tens-of-KB debug frames). So the targeted fix is: trampoline
alt-body emission onto the existing work stack (value_to_heap hylo precedent)
— NOT a blanket conversion of the whole emit family. stacker::maybe_grow at
the spine remains the cheap interim insurance if large-stack discipline ever
slips.
