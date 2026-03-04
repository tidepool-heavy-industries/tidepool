# Plan 6: Add capacity hints to LetRec emission allocations

## Problem

`tidepool-codegen/src/emit/expr.rs` allocates multiple `Vec::new()` and `HashMap::new()` in the LetRec emission path without capacity hints. For large LetRec groups (100+ bindings, common in GHC Core `-O2` output), these grow through many reallocations. The binding count is known upfront.

## Files to modify

- `tidepool-codegen/src/emit/expr.rs` — LetRec handling in `emit_node_with_lets()` (starts ~line 764)

## Implementation

### Specific allocations to fix

All in `emit_node_with_lets()`, inside the `CoreFrame::LetRec { bindings, body }` arm:

1. **Line 767** — `bindings.iter().partition(...)` returns two `Vec<_>`. The `.partition()` already returns properly sized Vecs — no change needed.

2. **Line 950** — `let mut deferred_simple = Vec::new();`
   → `let mut deferred_simple = Vec::with_capacity(simple_bindings.len());`

3. **Line 975-978** — `let mut pending_capture_updates: std::collections::HashMap<VarId, Vec<(cranelift_codegen::ir::Value, i32)>> = std::collections::HashMap::new();`
   → `... = std::collections::HashMap::with_capacity(rec_bindings.len());`

4. Search for other `Vec::new()` in the same function body between lines 764-1400 that could benefit from capacity hints. Key ones:
   - Any Vec collecting pre_allocs results → `with_capacity(rec_bindings.len())`
   - Deferred Con field tracking → `with_capacity(rec_bindings.len())`

### What NOT to change

- Line 735: `let mut let_cleanup: Vec<LetCleanup> = Vec::new();` — this is the outer loop accumulator that grows across multiple let-nesting levels. Capacity is unpredictable; leave as-is.
- Any Vec inside the Lam/Con pre-allocation loop body where the size depends on per-binding free variables (dynamic).

### Approach

Read lines 764-1400 of `emit/expr.rs` carefully. For each `Vec::new()` or `HashMap::new()`:
1. Determine what fills it
2. Check if the capacity is knowable from an outer collection's `.len()`
3. If yes, add `.with_capacity(n)`

This is a purely mechanical, low-risk change.

## Verification

```bash
cargo test --workspace
```

All tests pass. No behavioral change — only allocation efficiency improvement.
