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

## Notes

- Quantified margins (frame sizes before/after the frame-diet fix, observed
  depths) land in the sigsegv-hunt final report — fold them in here.
- The frame-diet fix (#[inline(never)] wrapper-path split) restores the pre-fix
  margin but guards an emergent property; stacker is what makes the cliff
  structurally unreachable.
