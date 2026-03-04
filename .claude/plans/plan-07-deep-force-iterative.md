# Plan 7: Convert deep_force() from recursion to iterative worklist

## Problem

`tidepool-eval/src/eval.rs:69-91` — `deep_force()` recursively walks Value trees without depth limit. It recurses on:
- `ThunkRef` → force then recurse (line 72-73)
- `Con` fields → recurse each field (line 77-78)
- `ConFun` args → recurse each arg (line 84-85)

The tree-walking evaluator overflows the Rust stack at ~50 iterations (documented in MEMORY.md). A 1000-element list is a 1000-deep nested Con chain — guaranteed stack overflow.

## Current code

```rust
pub fn deep_force(val: Value, heap: &mut dyn Heap) -> Result<Value, EvalError> {
    match val {
        Value::ThunkRef(id) => {
            let forced = force(Value::ThunkRef(id), heap)?;
            deep_force(forced, heap)
        }
        Value::Con(tag, fields) => {
            let mut forced_fields = Vec::with_capacity(fields.len());
            for f in fields {
                forced_fields.push(deep_force(f, heap)?);
            }
            Ok(Value::Con(tag, forced_fields))
        }
        Value::ConFun(tag, arity, args) => {
            let mut forced_args = Vec::with_capacity(args.len());
            for a in args {
                forced_args.push(deep_force(a, heap)?);
            }
            Ok(Value::ConFun(tag, arity, forced_args))
        }
        other => Ok(other),
    }
}
```

## Files to modify

- `tidepool-eval/src/eval.rs` — `deep_force()` function (line 69-91)
- `tidepool-eval/src/error.rs` — add `DepthLimit` variant to `EvalError`

## Implementation

### Step 1: Add `DepthLimit` to EvalError

In `tidepool-eval/src/error.rs`, add variant (after line 58):
```rust
/// Recursion depth limit exceeded during deep_force
DepthLimit,
```

Add Display arm:
```rust
EvalError::DepthLimit => write!(f, "recursion depth limit exceeded"),
```

### Step 2: Rewrite deep_force as iterative

The key insight: `deep_force` does a post-order traversal — force all children, then rebuild the parent. Use an explicit work stack.

```rust
pub fn deep_force(val: Value, heap: &mut dyn Heap) -> Result<Value, EvalError> {
    const MAX_DEPTH: usize = 100_000;

    enum Work {
        Force(Value),
        BuildCon(usize, usize),        // (tag, num_fields)
        BuildConFun(usize, usize, usize), // (tag, arity, num_args)
    }

    let mut stack: Vec<Work> = vec![Work::Force(val)];
    let mut results: Vec<Value> = Vec::new();

    while let Some(work) = stack.pop() {
        if stack.len() > MAX_DEPTH {
            return Err(EvalError::DepthLimit);
        }
        match work {
            Work::Force(v) => match v {
                Value::ThunkRef(id) => {
                    let forced = force(Value::ThunkRef(id), heap)?;
                    stack.push(Work::Force(forced));
                }
                Value::Con(tag, fields) => {
                    let n = fields.len();
                    stack.push(Work::BuildCon(tag, n));
                    // Push fields in reverse so they're processed in order
                    for f in fields.into_iter().rev() {
                        stack.push(Work::Force(f));
                    }
                }
                Value::ConFun(tag, arity, args) => {
                    let n = args.len();
                    stack.push(Work::BuildConFun(tag, arity, n));
                    for a in args.into_iter().rev() {
                        stack.push(Work::Force(a));
                    }
                }
                other => results.push(other),
            },
            Work::BuildCon(tag, n) => {
                let start = results.len() - n;
                let fields = results.split_off(start);
                results.push(Value::Con(tag, fields));
            }
            Work::BuildConFun(tag, arity, n) => {
                let start = results.len() - n;
                let args = results.split_off(start);
                results.push(Value::ConFun(tag, arity, args));
            }
        }
    }

    Ok(results.pop().expect("deep_force produced no result"))
}
```

### Notes

- `Value::Con` has `tag: usize` and `fields: Vec<Value>` — check the actual type definition in `tidepool-eval/src/value.rs` to confirm field types
- `force()` (line 39-64) remains recursive but is bounded by the `BlackHole` cycle detection — it only recurses on `ThunkRef → Evaluated(ThunkRef)` chains, which are shallow in practice
- The `MAX_DEPTH` of 100,000 is generous — heap-allocated work stack can handle this easily vs. the ~50 frame Rust stack limit

## Verification

```bash
cargo test --workspace
```

All tests pass. The behavioral change: deep structures that would have stack-overflowed now either succeed (if under 100K depth) or return a clean `EvalError::DepthLimit`.
