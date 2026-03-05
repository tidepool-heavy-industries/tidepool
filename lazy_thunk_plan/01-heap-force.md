# WS1a: `heap_force` TAG_THUNK Handling

## File
`tidepool-codegen/src/host_fns.rs` — function `heap_force` (lines 255-287)

## Current State

```rust
// line 267-268
if tag != layout::TAG_CLOSURE {
    return obj; // Thunk (tag=1) or unknown - not handled here
}
```

TAG_THUNK objects are returned unevaluated.

## Change

Replace the early return with a TAG_THUNK branch. The thunk state machine:

```
Unevaluated (0) → BlackHole (1) → Evaluated (2)
                                   ↓
                              return indirection
```

### Pseudocode

```rust
if tag == layout::TAG_THUNK {
    let state = *obj.add(layout::THUNK_STATE_OFFSET);
    match state {
        THUNK_UNEVALUATED => {
            // 1. Eager blackhole (plain byte store — single-threaded, no CAS)
            *obj.add(layout::THUNK_STATE_OFFSET) = layout::THUNK_BLACKHOLE;

            // 2. Read code pointer (shares offset 16 with indirection)
            let code_ptr = *(obj.add(layout::THUNK_CODE_PTR_OFFSET) as *const usize);

            // 3. Call thunk entry function
            //    Signature: fn(vmctx, thunk_ptr) -> whnf_ptr
            //    Thunk entry loads captures from thunk_ptr + 24 + 8*i
            let f: extern "C" fn(*mut VMContext, *mut u8) -> *mut u8 =
                std::mem::transmute(code_ptr);
            let result = f(vmctx, obj);

            // 4. Write indirection (offset 16, overwriting code_ptr)
            *(obj.add(layout::THUNK_INDIRECTION_OFFSET) as *mut *mut u8) = result;

            // 5. Set state = Evaluated
            *obj.add(layout::THUNK_STATE_OFFSET) = layout::THUNK_EVALUATED;

            result
        }
        THUNK_BLACKHOLE => {
            // Infinite loop detected (thunk forcing itself).
            // Same pattern as case exhaustion trap.
            // Call runtime_error or trigger trap.
            crate::host_fns::runtime_case_trap(vmctx);
            std::ptr::null_mut() // unreachable
        }
        THUNK_EVALUATED => {
            // Fast path: return cached result
            *(obj.add(layout::THUNK_INDIRECTION_OFFSET) as *const *mut u8)
        }
        _ => obj // Unknown state — defensive return
    }
}
```

## Calling Convention

Thunk entry functions have a **different signature** from closure entry:

| | Closures | Thunk entries |
|---|---|---|
| Signature | `fn(vmctx, closure_ptr, arg_ptr) -> result` | `fn(vmctx, thunk_ptr) -> result` |
| Args | 3 (vmctx, self, argument) | 2 (vmctx, self) |
| Self-update | No (closures don't memoize) | Yes (writes indirection + state) |

Wait — actually, looking at the current closure calling convention more carefully:
the closure call in `heap_force` passes `std::ptr::null_mut()` as arg (line 285).
Thunk entries genuinely take no argument. We can either:

**Option A**: Use a 2-arg signature `fn(vmctx, thunk_ptr) -> result`
**Option B**: Reuse the 3-arg signature with null arg (match closures)

Option A is cleaner. The thunk entry function knows it's a thunk.

## BlackHole Error Path

Use `runtime_case_trap` or a similar function that sets the thread-local error
flag and returns null. The JIT machine loop converts null results to
`JitError::Yield(UserError)`. The MCP server reports "infinite loop detected"
or similar.

Alternatively, add a new `runtime_blackhole_trap(vmctx)` for a distinct error
message.

## Testing (standalone, before codegen)

Can be tested by manually constructing thunk heap objects in a test:

1. Allocate a buffer with TAG_THUNK layout
2. Set state = Unevaluated
3. Set code_ptr to a simple test function (returns a Lit heap object)
4. Set captures
5. Call `heap_force` → verify it returns the Lit, state is Evaluated
6. Call `heap_force` again → verify fast path returns same Lit
7. Test BlackHole: set state = BlackHole, call `heap_force` → verify trap

## Estimated Size
~40 lines of new code + ~60 lines of tests
