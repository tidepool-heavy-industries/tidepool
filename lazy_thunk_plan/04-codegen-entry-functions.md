# WS2b: Thunk Entry Function Compilation

## The Hard Problem

This is the core technical challenge. A thunk entry function is a separate
Cranelift `ir::Function` that:
1. Receives `(vmctx: i64, thunk_ptr: i64)` as arguments
2. Loads captured variables from `thunk_ptr + 24 + 8*i`
3. Evaluates the deferred expression
4. Returns the WHNF heap pointer

This is structurally identical to how **closure bodies** are compiled — each
lambda body is already a separate Cranelift function with captures loaded from
the closure pointer. The thunk entry pattern is: "a zero-argument closure that
self-updates."

## Existing Closure Compilation Pattern

Study these files to understand the pattern to replicate:

- `tidepool-codegen/src/emit/expr.rs` — `EmitFrame::Lam` handler
  - How a lambda body is compiled as a separate function
  - How free variables are identified and captured
  - How the closure object is allocated

- `tidepool-codegen/src/emit/mod.rs` or `pipeline.rs`
  - How multiple Cranelift functions are managed in one compilation unit
  - How function references (code pointers) are obtained for closures

- `tidepool-codegen/src/jit_machine.rs`
  - How compiled functions are registered and callable

## Free Variable Analysis

For a thunk body (a sub-expression at a given node index in the CoreExpr tree),
we need to know which variables are "free" — referenced by the sub-expression
but defined outside it. These become the thunk's captures.

**Check if free variable analysis already exists.** The closure compilation
must already do this (to know what to capture). If so, reuse it. If not,
implement:

```rust
fn free_vars(idx: usize, expr: &CoreExpr, bound: &HashSet<VarId>) -> Vec<VarId> {
    match &expr.nodes[idx] {
        CoreFrame::Var(v) => {
            if bound.contains(v) { vec![] } else { vec![*v] }
        }
        CoreFrame::Lam { binder, body } => {
            let mut bound = bound.clone();
            bound.insert(*binder);
            free_vars(*body, expr, &bound)
        }
        CoreFrame::App { func, arg } => {
            let mut fvs = free_vars(*func, expr, bound);
            fvs.extend(free_vars(*arg, expr, bound));
            fvs.sort();
            fvs.dedup();
            fvs
        }
        // ... etc for all CoreFrame variants
    }
}
```

## Thunk Entry Function Structure

```
fn thunk_entry_NNN(vmctx: i64, thunk_ptr: i64) -> i64 {
    // Load captures
    let capture_0 = load(thunk_ptr + 24);
    let capture_1 = load(thunk_ptr + 32);
    // ...

    // Bind captures to their VarIds in the local environment
    env[var_id_0] = capture_0;
    env[var_id_1] = capture_1;

    // Evaluate the deferred expression (reuse emit_node / emit_expr)
    let result = <emit the sub-expression>;

    // Self-update: write indirection + set state
    // NOTE: This could be done here OR in heap_force.
    // Doing it in heap_force is simpler (one place) and the thunk entry
    // function just returns the result. heap_force writes the indirection.
    // This is the recommended approach.

    return result;
}
```

**Key decision: Who writes the indirection?**

Option A: Thunk entry writes it (self-updating thunk)
- Pro: Thunk entry has the thunk pointer, can write directly
- Con: Every thunk entry must include update code (code bloat)

Option B: `heap_force` writes it after calling the entry function
- Pro: Update logic in one place, thunk entries are simpler
- Con: `heap_force` already has the thunk pointer, this is natural

**Recommend Option B.** The thunk entry function is a pure computation:
`fn(vmctx, thunk_ptr) -> whnf_ptr`. The `heap_force` function handles
the state machine (blackhole, call entry, write indirection, set evaluated).
This separates concerns cleanly.

## Compilation Pipeline Integration

The thunk entry function must be compiled as part of the same compilation unit
as the parent function. The flow:

1. During Con field emission, detect non-trivial field
2. Identify free variables of the sub-expression
3. Create a new Cranelift `ir::Function` for the thunk entry
4. Compile the sub-expression within that function context
5. Finalize the function, obtain its code pointer
6. Back in the parent function, use the code pointer when allocating the thunk

This requires the compilation pipeline to support "spawning" new functions
mid-compilation. Check how the existing closure compilation does this — it
likely uses a queue or deferred compilation list.

## Recursive Thunks

The critical case: `repeat x = x : repeat x`. The tail field `repeat x` is
non-trivial (it's an App). Its thunk body will contain a call to `repeat`,
which produces another Con with another thunk tail. This creates the lazy
chain:

```
(:) x <thunk: repeat x>
         ↓ (when forced)
       (:) x <thunk: repeat x>
                  ↓ (when forced)
                (:) x <thunk: repeat x>
                ...
```

Each thunk entry allocates a new Con + a new thunk. The chain extends
one step per force. `take 5 (repeat 1)` forces 5 times, producing 5 cons
cells + 5 intermediate thunks (which become indirections, cleaned up by GC).

This works naturally with no special handling — the thunk entry function
just calls the original function, which returns a new Con.

## Estimated Size
~150-200 lines for thunk entry compilation + free variable analysis
This is the largest single component.
