# Plan: Extract shared setup boilerplate from emit_lam and emit_thunk

## Goal

`emit_lam` and `emit_thunk` in `tidepool-codegen/src/emit/expr.rs` share ~35 lines of identical setup boilerplate. Extract a helper.

## Duplicated Code

Both functions (lines 645-680 and 850-880) do:
1. `tree.extract_subtree(body_idx)` to get the body
2. `free_vars::free_vars(&body_tree)` to compute free variables
3. Filter out vars not in `ctx.env`, log dropped vars
4. Sort remaining fvs by `VarId.0`

The only difference: `emit_lam` removes `binder` from fvs (line 658: `fvs.remove(&binder)`).

## Plan

### Extract helper in `emit/expr.rs`

```rust
/// Compute sorted capture list for a closure/thunk body.
/// Returns (body_tree, sorted_captures).
/// If `exclude` is Some, that VarId is removed from free vars (for lambda binders).
fn compute_captures(
    ctx: &EmitContext,
    tree: &CoreExpr,
    body_idx: usize,
    exclude: Option<VarId>,
    label: &str,
) -> (CoreExpr, Vec<VarId>) {
    let body_tree = tree.extract_subtree(body_idx);
    let mut fvs = tidepool_repr::free_vars::free_vars(&body_tree);
    if let Some(binder) = exclude {
        fvs.remove(&binder);
    }
    let dropped: Vec<VarId> = fvs
        .iter()
        .filter(|v| !ctx.env.contains_key(v))
        .copied()
        .collect();
    if !dropped.is_empty() {
        ctx.trace_scope(&format!(
            "{} capture: dropped {} free vars not in scope: {:?}",
            label, dropped.len(), dropped
        ));
    }
    let mut sorted_fvs: Vec<VarId> = fvs
        .into_iter()
        .filter(|v| ctx.env.contains_key(v))
        .collect();
    sorted_fvs.sort_by_key(|v| v.0);
    (body_tree, sorted_fvs)
}
```

### Update call sites

In `emit_lam`:
```rust
let (body_tree, sorted_fvs) = compute_captures(ctx, tree, body_idx, Some(binder), "lam");
```

In `emit_thunk`:
```rust
let (body_tree, sorted_fvs) = compute_captures(ctx, tree, body_idx, None, "thunk");
```

## Verify

```bash
cargo check -p tidepool-codegen
cargo test -p tidepool-codegen
```

## Boundary

- Do NOT change any capture computation logic
- Do NOT touch the rest of emit_lam/emit_thunk (inner function creation, etc.)
- The helper is private to expr.rs
