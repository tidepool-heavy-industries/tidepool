# Lazy Thunks for Con Fields — Implementation Plan

## Problem

The JIT evaluates all data constructor fields eagerly before allocation.
`x : xs` evaluates `xs` immediately — if `xs` is a recursive call producing
more cons cells, it's infinite recursion → SIGSEGV. GHC's build/foldr fusion
saves some cases (`take 5 [0..]` works) but unfused patterns like
`zipWith f xs [0..]` crash.

## Design Principles

1. **Match GHC semantics** — we consume GHC Core, so our evaluation should
   match GHC's lazy-by-default for Con fields.
2. **Exploit purity** — all thunks are pure (effects are free monad values,
   not side effects). No update frames, no exception thunks, no atomics.
   Single-threaded. Eager blackholing is a plain byte store.
3. **Self-updating entry** — thunk entry functions are purpose-built Cranelift
   functions that receive their own pointer, compute, write the indirection,
   and return. No stack-based update frames.
4. **Cheapness gate** — trivial expressions (Var, Lit) are never thunkified.
   Only non-trivial fields (App, Case, recursive calls) become thunks.

## Existing Infrastructure

The thunk infrastructure is ~80% present:

| Component | Status | File |
|-----------|--------|------|
| `HeapTag::Thunk = 1` | ✅ Defined | `tidepool-heap/src/layout.rs:7` |
| `ThunkStateTag` enum (Unevaluated/BlackHole/Evaluated) | ✅ Defined | `layout.rs:62-66` |
| Thunk layout constants (state@8, code_ptr@16, indirection@16, captures@24+) | ✅ Defined | `layout.rs:177-185` |
| GC tracing for thunks (`for_each_pointer_field` TAG_THUNK branch) | ✅ Implemented | `gc/raw.rs:68-84` |
| Case scrutinee forcing (`tag < 2` check) | ✅ Implemented | `emit/case.rs:119-159` |
| `heap_force` TAG_THUNK handling | ❌ Returns obj unchanged | `host_fns.rs:267-268` |
| Thunk creation in Con emission | ❌ All fields eager | `emit/expr.rs:240-278` |
| Thunk entry function compilation | ❌ Does not exist | — |
| `heap_to_value` TAG_THUNK | ❌ Not handled | `heap_bridge.rs` |

## Workstreams

Three independent workstreams that converge at integration:

### WS1: heap_force + heap_bridge (~15% of effort)
- Add TAG_THUNK branch to `heap_force` in `host_fns.rs`
- Add TAG_THUNK case to `heap_to_value` in `heap_bridge.rs`
- Can be tested with hand-crafted thunk objects + simple test functions
- See: `01-heap-force.md`, `02-heap-bridge.md`

### WS2: Codegen — thunk creation + entry functions (~70% of effort)
- Cheapness analysis to decide which Con fields to thunkify
- Free variable analysis for thunk bodies
- Compile deferred expressions as separate Cranelift functions
- Allocate thunk heap objects with captures
- See: `03-codegen-thunk-creation.md`, `04-codegen-entry-functions.md`

### WS3: Tests + integration (~15% of effort)
- Haskell test fixtures for infinite lists
- Integration tests in `tidepool-eval/tests/`
- MCP eval smoke tests
- See: `05-tests.md`

## Dependency Graph

```
WS1 (heap_force + bridge) ──────────────┐
                                         ├──→ Integration tests
WS2 (codegen: creation + entry fns) ─────┘

WS3 (test fixtures) ── can start immediately, runs last
```

WS1 and the test fixture portion of WS3 can proceed in parallel with WS2.
Integration testing requires all three.

## Key Insight: GC Already Works

The Cheney collector's `for_each_pointer_field` in `gc/raw.rs:68-84` already
handles TAG_THUNK with state-dependent tracing:
- Unevaluated: trace captures (derived from size: `(size - 24) / 8`)
- Evaluated: trace indirection pointer only
- BlackHole: nothing to trace

**No GC changes needed.** This is the biggest scope reduction from the initial estimate.

## Out of Scope (v2+)

- Pointer tagging (LSB of heap pointers → skip force check)
- Single-entry thunks (skip memoization for OneOcc bindings)
- Selector thunk GC evaluation
- Stateless thunks (re-evaluate small bodies instead of memoizing)
- Optimistic evaluation (fuel-budgeted speculative eagerness)
