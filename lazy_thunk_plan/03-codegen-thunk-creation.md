# WS2a: Thunk Creation in Con Field Emission

## File
`tidepool-codegen/src/emit/expr.rs` — `EmitFrame::Con` handler (lines 240-278)

## Current State

All Con fields are eagerly evaluated before the constructor is allocated:

```rust
EmitFrame::Con { tag, fields } => {
    let field_vals: Vec<Value> = fields
        .iter()
        .map(|v| ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, *v))
        .collect();
    // ... allocate Con, store all field_vals ...
}
```

This is the root cause: `ensure_heap_ptr` recursively evaluates every field,
which for `(:) x (f x)` means evaluating `(f x)` immediately.

## Change

Replace eager field evaluation with a cheapness-gated strategy:

```
for each field in Con.fields:
    if is_trivial(field):
        evaluate eagerly (existing path)
    else:
        compile field as thunk entry function
        allocate thunk with captures
        use thunk pointer as field value
```

## Cheapness Analysis

An expression is **trivial** (safe to evaluate eagerly) if it:
- Is a `Var` that's already bound in the current environment
- Is a `Lit` (literal value)
- Is a `Con` where ALL fields are themselves trivial (nested trivial Con)

Everything else is **non-trivial** and must be thunkified:
- `App` (function application — might diverge or be expensive)
- `Case` (pattern match — might force other thunks)
- `PrimOp` application (might involve computation)
- `Lam` (creates a closure — trivial to allocate, but the body is deferred)
  Actually: Lam creates a closure which IS a heap value already. A Lam in
  field position means "this field is a function value." That's already a
  heap pointer. **Lam is trivial** — it's already WHNF.

### Implementation

Add a function `is_trivial_field(node_idx: usize, expr: &CoreExpr) -> bool`:

```rust
fn is_trivial_field(idx: usize, expr: &CoreExpr) -> bool {
    match &expr.nodes[idx] {
        CoreFrame::Var(_) => true,
        CoreFrame::Lit(_) => true,
        CoreFrame::Lam { .. } => true,  // Already WHNF (closure)
        CoreFrame::Con { fields, .. } => {
            fields.iter().all(|&f| is_trivial_field(f, expr))
        }
        _ => false,  // App, Case, PrimOp, LetNonRec, LetRec, Join, Jump
    }
}
```

## Thunk Allocation

For a non-trivial field, instead of evaluating, emit:

1. **Identify free variables** of the sub-expression (see `04-codegen-entry-functions.md`)
2. **Compile thunk entry** as a separate Cranelift function (see `04-codegen-entry-functions.md`)
3. **Allocate thunk object**:

```
Thunk layout:
  [0]    TAG_THUNK (1 byte)
  [1-2]  size (u16) = 24 + 8 * num_captures
  [8]    state = THUNK_UNEVALUATED (0)
  [16]   code_ptr → thunk entry function
  [24]   capture[0]
  [32]   capture[1]
  ...
```

Cranelift emission:
```rust
let num_captures = free_vars.len();
let thunk_size = 24 + 8 * num_captures as u64;
let ptr = emit_alloc_fast_path(builder, vmctx, thunk_size, gc_sig, oom_func);

// Header
let tag_val = builder.ins().iconst(types::I8, layout::TAG_THUNK as i64);
builder.ins().store(MemFlags::trusted(), tag_val, ptr, 0);
let size_val = builder.ins().iconst(types::I16, thunk_size as i64);
builder.ins().store(MemFlags::trusted(), size_val, ptr, 1);

// State = Unevaluated
let state_val = builder.ins().iconst(types::I8, layout::THUNK_UNEVALUATED as i64);
builder.ins().store(MemFlags::trusted(), state_val, ptr, THUNK_STATE_OFFSET);

// Code pointer
let code_ptr = /* function reference to thunk entry */;
builder.ins().store(MemFlags::trusted(), code_ptr, ptr, THUNK_CODE_PTR_OFFSET);

// Captures
for (i, &fv) in free_vars.iter().enumerate() {
    let fv_val = env.lookup(fv);  // SSA value for this free variable
    let fv_ptr = ensure_heap_ptr(builder, vmctx, gc_sig, oom_func, fv_val);
    builder.ins().store(
        MemFlags::trusted(),
        fv_ptr,
        ptr,
        (THUNK_CAPTURED_OFFSET + 8 * i) as i32,
    );
}

builder.declare_value_needs_stack_map(ptr);
// Use ptr as the Con field value
```

## Interaction with LetRec 5-Phase

The LetRec mechanism (lines 784-1373 in expr.rs) pre-allocates Cons with NULL
fields and fills them incrementally. This handles **data recursion** (e.g.,
`let xs = 1 : xs`). Runtime thunks handle **computation recursion** (e.g.,
`zipWith f xs [0..]`).

The two are orthogonal:
- In LetRec, Con fields that reference other LetRec bindings use the existing
  deferred-fill mechanism (phases 3b-3d).
- In non-LetRec context, Con fields that are non-trivial become thunks.
- In LetRec, Con fields that are non-trivial AND don't reference other LetRec
  bindings could also become thunks, but this is an edge case for v1.

## Risk: Evaluation Order Changes

Thunkifying Con fields changes evaluation order. Code that accidentally
depended on eager field evaluation might behave differently. Mitigations:
- The test suite catches regressions
- GHC Core semantics are lazy-by-default, so our eager evaluation was the
  deviation — thunks make us MORE correct
- Effects are values, so evaluation order of pure code doesn't affect
  observable behavior

## Estimated Size
~80 lines for cheapness analysis + thunk allocation
Depends on: `04-codegen-entry-functions.md`
