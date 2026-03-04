# Plan 4: Convert emit panic!() to Result errors

## Problem

`tidepool-codegen/src/emit/expr.rs` has two `panic!()` calls in the codegen hot path:

1. **Line 451** — Lambda capture phase: `panic!("Lam capture: VarId({:#x}) not in env. env keys: {:?}", ...)`
2. **Line 1318** — LetRec Phase 3a' capture fill: `panic!("LetRec capture fill: VarId({:#x}) not in env after Phase 3c.", ...)`

These are compiler bugs (missing VarId in env), but they surface as panics that kill the eval thread. `catch_unwind` in the MCP layer catches them, but the error message is opaque ("panicked at..."). Converting to `Result<_, EmitError>` gives clean error messages and avoids unwind through unsafe code.

## Files to modify

- `tidepool-codegen/src/emit/mod.rs` — `EmitError` enum (line 65-71) and `Display` impl (line 73-84)
- `tidepool-codegen/src/emit/expr.rs` — lines 450-459 and 1317-1322

## Implementation

### Step 1: Add `MissingCaptureVar` variant to `EmitError`

In `tidepool-codegen/src/emit/mod.rs`, add to the enum (after line 70):

```rust
pub enum EmitError {
    UnboundVariable(VarId),
    NotYetImplemented(String),
    CraneliftError(String),
    Pipeline(crate::pipeline::PipelineError),
    InvalidArity(PrimOpKind, usize, usize),
    /// A variable needed for closure capture was not found in the environment.
    MissingCaptureVar(VarId, String),
}
```

Add Display arm (after line 82):
```rust
EmitError::MissingCaptureVar(v, ctx) => {
    write!(f, "missing capture variable VarId({:#x}): {}", v.0, ctx)
}
```

### Step 2: Replace panic at line 450-459

Current code:
```rust
let captures: Vec<(VarId, SsaVal)> = sorted_fvs
    .iter()
    .map(|v| {
        let val = ctx.env.get(v).unwrap_or_else(|| {
            panic!(
                "Lam capture: VarId({:#x}) not in env. env keys: {:?}",
                v.0,
                ctx.env.keys().map(|k| format!("{:#x}", k.0)).collect::<Vec<_>>()
            )
        });
        (*v, *val)
    })
    .collect();
```

Replace with:
```rust
let captures: Vec<(VarId, SsaVal)> = sorted_fvs
    .iter()
    .map(|v| {
        let val = ctx.env.get(v).ok_or_else(|| {
            EmitError::MissingCaptureVar(
                *v,
                format!("Lam capture: not in env (env has {} vars)", ctx.env.len()),
            )
        })?;
        Ok((*v, *val))
    })
    .collect::<Result<Vec<_>, _>>()?;
```

### Step 3: Replace panic at line 1317-1322

Current code:
```rust
let ssaval = self.env.get(&var_id).unwrap_or_else(|| {
    panic!(
        "LetRec capture fill: VarId({:#x}) not in env after Phase 3c.",
        var_id.0
    );
});
```

Replace with:
```rust
let ssaval = self.env.get(&var_id).ok_or_else(|| {
    EmitError::MissingCaptureVar(
        var_id,
        "LetRec Phase 3a' capture fill: not in env after Phase 3c".into(),
    )
})?;
```

## Verification

```bash
cargo test --workspace
```

All tests should pass. The error path is only hit on compiler bugs (missing VarId), which don't occur in any test case. The change makes the error recoverable instead of fatal.
