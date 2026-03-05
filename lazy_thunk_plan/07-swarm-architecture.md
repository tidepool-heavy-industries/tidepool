# Swarm Execution Architecture

## Workstream Decomposition

```
                    ┌─────────────────────┐
                    │   TL (coordinator)   │
                    └──────────┬──────────┘
                               │
              ┌────────────────┼────────────────┐
              │                │                │
     ┌────────▼────────┐ ┌────▼────────┐ ┌─────▼──────┐
     │ WS1: Force+Bridge│ │ WS2: Codegen│ │ WS3: Tests │
     │   (worktree)    │ │  (worktree) │ │ (worktree) │
     └────────┬────────┘ └──────┬──────┘ └─────┬──────┘
              │                 │               │
              │           ┌─────┴─────┐         │
              │           │           │         │
              │     ┌─────▼───┐ ┌─────▼───┐     │
              │     │ WS2a:   │ │ WS2b:   │     │
              │     │ Creation│ │ Entry   │     │
              │     │ (sub)   │ │ (sub)   │     │
              │     └─────────┘ └─────────┘     │
              │                                 │
              └──────────────┬──────────────────┘
                             │
                    ┌────────▼────────┐
                    │  Integration    │
                    │  (main branch)  │
                    └─────────────────┘
```

## Worktree Assignments

### Worktree 1: WS1 — heap_force + heap_bridge
**Branch**: `main.lazy-thunks.ws1-force`
**Agent**: Gemini leaf (spawn_leaf_subtree)
**Scope**:
- `tidepool-codegen/src/host_fns.rs` — TAG_THUNK branch in heap_force
- `tidepool-codegen/src/heap_bridge.rs` — TAG_THUNK case in heap_to_value
- Unit tests for both
**Estimated**: ~60 lines code + ~80 lines tests
**Dependencies**: None (can start immediately)
**Acceptance criteria**:
- `heap_force` handles Unevaluated → BlackHole → call entry → write indirection → Evaluated
- `heap_force` handles Evaluated → return indirection (fast path)
- `heap_force` handles BlackHole → trap
- `heap_to_value` follows indirection for Evaluated thunks
- Unit tests pass
- `cargo check --workspace` passes

### Worktree 2: WS2 — Codegen (thunk creation + entry functions)
**Branch**: `main.lazy-thunks.ws2-codegen`
**Agent**: Claude sub-TL (spawn_subtree) — this is the hard problem, needs
architectural judgment
**Scope**:
- `tidepool-codegen/src/emit/expr.rs` — cheapness analysis + thunk allocation
  in Con handler
- New or extended file for thunk entry function compilation
- Free variable analysis
**Estimated**: ~200-300 lines
**Dependencies**: None for development, but integration requires WS1
**Sub-decomposition** (if sub-TL wants to parallelize):
- WS2a: Cheapness analysis + thunk allocation (the Con handler changes)
- WS2b: Thunk entry function compilation (separate Cranelift functions)

**Acceptance criteria**:
- Non-trivial Con fields emit thunk objects instead of eager evaluation
- Thunk entry functions correctly load captures and evaluate deferred expressions
- Trivial fields (Var, Lit, Lam) remain eagerly evaluated
- `cargo check --workspace` passes
- Existing tests still pass (no regressions from evaluation order change)

### Worktree 3: WS3 — Test fixtures
**Branch**: `main.lazy-thunks.ws3-tests`
**Agent**: Gemini leaf (spawn_leaf_subtree)
**Scope**:
- `haskell/test/Suite.hs` — new test bindings for infinite lists, zipWith,
  BlackHole detection
- Compile to CBOR fixtures
- `tidepool-eval/tests/haskell_suite.rs` — new test cases
**Estimated**: ~100 lines Haskell + ~50 lines Rust
**Dependencies**: None for fixture creation. Integration tests require WS1 + WS2.
**Acceptance criteria**:
- New Haskell test bindings compile and produce CBOR
- Test runner code is written (may initially fail until WS1+WS2 land)

## Integration Phase

After all three worktrees have PRs:

1. Merge WS1 (force + bridge) into main
2. Merge WS3 (test fixtures) into main — tests expected to fail
3. Merge WS2 (codegen) into main — tests should now pass
4. Run full regression: `cargo test --workspace`
5. Rebuild MCP server: `cargo install --path tidepool`
6. MCP smoke tests (Level 3 from 05-tests.md)
7. Doc updates (06-docs.md)

## Risk Mitigation

**Biggest risk**: WS2 (codegen) is complex and might need iteration. The
sub-TL should:
- Start by studying existing closure compilation thoroughly
- Get a minimal thunk working for a single simple case first
- Then generalize

**Regression risk**: Thunkifying fields changes eval order. All 100+ existing
test bindings must still pass. If any break, the cheapness analysis may need
to be more conservative (thunkify fewer things).

**GC risk**: Thunks add a new object type to the live heap. The GC already
handles TAG_THUNK in `for_each_pointer_field`, but edge cases (thunks as
GC roots, thunks in stack maps) need testing under GC pressure.
