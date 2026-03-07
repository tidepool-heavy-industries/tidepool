# Plan: Extract shared tree traversal utilities from optimizer passes

## Goal

`get_children`, `replace_subtree`, and `rebuild` are duplicated identically across dce.rs, inline.rs, and case_reduce.rs (~240 lines total). Extract to a shared module.

## Current State

| Function | dce.rs | inline.rs | case_reduce.rs |
|----------|--------|-----------|----------------|
| `get_children` | 72-97 | 62-87 | 105-130 |
| `replace_subtree` | 99-111 | 89-101 | 157-169 |
| `rebuild` | 113-140 | 103-130 | 171-198 |

All three copies are **identical** in logic and signature. beta.rs has its own `replace_subtree`/`rebuild` (same logic) but no `get_children`.

Additionally, `case_reduce.rs:132-155` duplicates `extract_subtree` which already exists as a method on `RecursiveTree` in `tidepool-repr/src/tree.rs:14-44`.

## Plan

### 1. Add functions to `tidepool-repr/src/tree.rs`

Add two public functions after the existing `extract_subtree` method:

```rust
/// Get all child indices of a CoreFrame node.
pub fn get_children(frame: &CoreFrame<usize>) -> Vec<usize> {
    // ... (copy from any of the three identical implementations)
}

/// Replace the subtree rooted at `target_idx` with `replacement`.
pub fn replace_subtree(expr: &CoreExpr, target_idx: usize, replacement: &CoreExpr) -> CoreExpr {
    let mut new_nodes = Vec::new();
    let mut old_to_new = HashMap::new();
    rebuild(expr, 0, target_idx, replacement, &mut new_nodes, &mut old_to_new);
    RecursiveTree { nodes: new_nodes }
}

fn rebuild(
    expr: &CoreExpr,
    idx: usize,
    target: usize,
    replacement: &CoreExpr,
    new_nodes: &mut Vec<CoreFrame<usize>>,
    old_to_new: &mut HashMap<usize, usize>,
) -> usize {
    // ... (copy from any of the three identical implementations)
}
```

Re-export from `tidepool-repr/src/lib.rs`.

### 2. Update optimizer passes

In each of dce.rs, inline.rs, case_reduce.rs:
- Remove the local `get_children`, `replace_subtree`, `rebuild` functions
- Import from `tidepool_repr::{get_children, replace_subtree}` (or `tidepool_repr::tree::*`)

In case_reduce.rs:
- Remove local `extract_subtree` (lines 132-155)
- Use `expr.extract_subtree(root_idx)` which is already a method on `RecursiveTree`

In beta.rs:
- Remove local `replace_subtree`/`rebuild` (lines 123-169)
- Import from `tidepool_repr`

### 3. Check tidepool-repr Cargo.toml

Ensure `HashMap` import is available (it's `std::collections::HashMap`, so no new deps needed).

## Verify

```bash
cargo check -p tidepool-repr
cargo check -p tidepool-optimize
cargo test -p tidepool-optimize
```

## Boundary

- Do NOT change any optimization logic — only move code
- Do NOT change function signatures
- Do NOT add new dependencies
- `rebuild` should be private (`fn`, not `pub fn`) — it's an implementation detail of `replace_subtree`
