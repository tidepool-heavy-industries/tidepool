# Plan: Bundle recurring emit parameters into EmitSession struct

## Goal

The parameter tuple `(pipeline, builder, vmctx, gc_sig, oom_func, tree)` is passed to 14+ functions in the emit module. Bundle into a struct to shrink signatures.

## Current State

Every function in `tidepool-codegen/src/emit/` takes these same 6 params alongside `&mut EmitContext`:

```rust
fn emit_lam(
    ctx: &mut EmitContext,
    pipeline: &mut CodegenPipeline,     // ← these 6
    builder: &mut FunctionBuilder,      //
    vmctx: Value,                       //
    gc_sig: ir::SigRef,                 //
    oom_func: ir::FuncRef,              //
    tree: &CoreExpr,                    //
    binder: VarId,                      // ← per-call args
    body_idx: usize,
) -> Result<SsaVal, EmitError>
```

**Affected functions** (all in emit/expr.rs unless noted):
- `collapse_frame` (line 201, 8 params)
- `emit_subtree` (line 607, 8 params)
- `emit_lam` (line 645, 10 params)
- `emit_thunk` (line 850, 8 params)
- `emit_node` (line 1195, 8 params)
- `emit_tail_app` (line 1341, 8 params)
- `emit_letrec_phases` (line 1478, 8+ params)
- `letrec_post_simple_step` (line 2045, 8+ params)
- `letrec_finish_phases` (line 2107, 8+ params)
- `emit_primop` (emit/primop.rs:34, 7 params)
- `emit_case` (emit/case.rs:12, 8+ params)
- `emit_data_dispatch` (emit/case.rs:104, 8+ params)
- `emit_case_trap` (emit/case.rs:293, 6+ params)
- join emission functions (emit/join.rs:11, 101, 8+ params)

## Plan

### 1. Define EmitSession in `emit/mod.rs`

```rust
/// Bundles the per-function-compilation state that every emit helper needs.
pub struct EmitSession<'a> {
    pub pipeline: &'a mut CodegenPipeline,
    pub builder: &'a mut FunctionBuilder<'a>,
    pub vmctx: Value,
    pub gc_sig: ir::SigRef,
    pub oom_func: ir::FuncRef,
    pub tree: &'a CoreExpr,
}
```

Note: lifetime annotations may need adjustment based on how `FunctionBuilder` borrows. The builder borrows from a `Function` that lives in `pipeline`. May need to keep `builder` separate and pass `EmitSession` + `builder` to avoid self-referential borrow. Investigate actual lifetimes before committing to the struct layout.

**Alternative** (if lifetimes are tricky): keep `builder` as a separate param and bundle the other 4:

```rust
pub struct EmitSession<'a> {
    pub pipeline: &'a mut CodegenPipeline,
    pub vmctx: Value,
    pub gc_sig: ir::SigRef,
    pub oom_func: ir::FuncRef,
    pub tree: &'a CoreExpr,
}
```

This still cuts each signature by 4 params.

### 2. Update all 14+ functions

Change signatures from:
```rust
fn emit_lam(ctx, pipeline, builder, vmctx, gc_sig, oom_func, tree, binder, body_idx)
```
To:
```rust
fn emit_lam(ctx, sess: &mut EmitSession, builder: &mut FunctionBuilder, binder, body_idx)
```

### 3. Update all call sites

Each call site constructs or passes through the `EmitSession`.

## Verify

```bash
cargo check -p tidepool-codegen
cargo test -p tidepool-codegen
```

## Boundary

- Do NOT change any emit logic — purely mechanical parameter bundling
- Do NOT change the public API of the codegen crate
- Do NOT remove `#[allow(clippy::too_many_arguments)]` until the signatures actually shrink below the threshold
- If lifetime issues arise with FunctionBuilder, use the alternative layout (keep builder separate)
- `EmitContext` stays separate — it has different ownership semantics (mutated across the whole compilation, not per-function)
