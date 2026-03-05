# WS1b: `heap_to_value` TAG_THUNK Handling

## File
`tidepool-codegen/src/heap_bridge.rs` — function `heap_to_value_inner` (line 49+)

## Current State

The function dispatches on heap object tag: TAG_LIT (line 59), TAG_CON (line 133).
TAG_THUNK is not handled — falls through to an error.

## Change

Add a TAG_THUNK case that follows the indirection for evaluated thunks:

```rust
t if t == layout::TAG_THUNK => {
    let state = *ptr.add(layout::THUNK_STATE_OFFSET);
    match state {
        layout::THUNK_EVALUATED => {
            // Follow indirection pointer to the WHNF result
            let target = *(ptr.add(layout::THUNK_INDIRECTION_OFFSET) as *const *const u8);
            heap_to_value_inner(target, depth + 1)
        }
        layout::THUNK_UNEVALUATED => {
            // Shouldn't happen — the final result should be fully evaluated.
            // But defensively return an error.
            Err(BridgeError::UnevaluatedThunk)
        }
        layout::THUNK_BLACKHOLE => {
            Err(BridgeError::BlackHole)
        }
        _ => Err(BridgeError::UnknownThunkState(state))
    }
}
```

## BridgeError Extension

Add new variants to `BridgeError`:

```rust
pub enum BridgeError {
    // ... existing variants ...
    UnevaluatedThunk,
    BlackHole,
    UnknownThunkState(u8),
}
```

## Why This Is Needed

After the JIT finishes executing, the final result heap object is converted to
a `Value` via `heap_to_value` for serialization. If the result contains
evaluated thunks (e.g., a list where some tail cells are memoized thunks),
the bridge must follow the indirection chain to reach the actual data.

In practice, the JIT's `heap_force` should have already resolved any thunks
that appear in the final result (since they were forced by case scrutiny).
But defensive handling is important for robustness.

## Estimated Size
~20 lines of new code
