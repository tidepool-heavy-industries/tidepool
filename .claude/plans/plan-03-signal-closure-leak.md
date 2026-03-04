# Plan 3: Fix signal handler closure leak on JIT crash

## Problem

`tidepool-codegen/src/signal_safety.rs` — `with_signal_protection()` (line 92-128) boxes a closure into `Box<Box<dyn FnOnce()>>`, converts to raw pointer (line 114: `Box::into_raw(boxed)`), and passes it as `userdata` to the C trampoline. On normal return, the trampoline reconstructs and drops the Box (line 70). But when a signal fires, `siglongjmp` skips the trampoline's drop — the comment at line 121-122 says "the Box was leaked. We can't recover it safely." Each leak is ~48+ bytes. Over hundreds of JIT crashes, this accumulates.

## Files to modify

- `tidepool-codegen/src/signal_safety.rs` — `with_signal_protection()` and the thread-local section

## Implementation

### Step 1: Add thread-local slot for the closure pointer

After the existing `JMP_BUF` thread-local (line 62-64), add:

```rust
thread_local! {
    static CLOSURE_PTR: Cell<*mut libc::c_void> = const { Cell::new(ptr::null_mut()) };
}
```

### Step 2: Store pointer before sigsetjmp, recover on signal

In `with_signal_protection()`, after `Box::into_raw` (line 114):

```rust
let boxed: Box<Box<dyn FnOnce()>> = Box::new(wrapper);
let userdata = Box::into_raw(boxed) as *mut libc::c_void;

// Store so we can recover on signal
CLOSURE_PTR.with(|cell| cell.set(userdata));

let val = tidepool_sigsetjmp_call(&mut buf, trampoline, userdata);

// Clear regardless of outcome
CLOSURE_PTR.with(|cell| cell.set(null_mut()));
JMP_BUF.with(|cell| cell.set(null_mut()));

if val != 0 {
    // Signal was caught — recover and drop the leaked closure
    if !userdata.is_null() {
        // SAFETY: userdata was created by Box::into_raw above and was not
        // consumed by the trampoline (signal interrupted it).
        drop(Box::from_raw(userdata as *mut Box<dyn FnOnce()>));
    }
    return Err(SignalError(val));
}
```

Wait — there's a subtlety. After `siglongjmp`, we're back at the `sigsetjmp` return point. The local variable `userdata` is still valid (it was set before `sigsetjmp`). Actually, `sigsetjmp` returns twice — first returning 0 (normal path), then returning `val` (signal path). On the signal path, local variables modified between the two returns may be clobbered UNLESS they are `volatile`. But `userdata` was set BEFORE `sigsetjmp`, so it's safe — it wasn't modified between the two returns.

Actually, looking more carefully: `tidepool_sigsetjmp_call` is a C wrapper that calls `sigsetjmp` internally. The return value flow is: C wrapper calls `sigsetjmp`, if 0 calls the callback, returns 0. If signal fires, `siglongjmp` returns to the C `sigsetjmp` point, C wrapper returns the signal number. So from Rust's perspective, `tidepool_sigsetjmp_call` returns once — either 0 or the signal number. The `userdata` variable is still valid.

So the fix is simpler than using a thread-local — just drop `userdata` in the error path:

```rust
if val != 0 {
    // Signal was caught. The trampoline never ran (or was interrupted),
    // so the boxed closure was not consumed. Drop it to prevent leak.
    drop(Box::from_raw(userdata as *mut Box<dyn FnOnce()>));
    return Err(SignalError(val));
}
```

The `CLOSURE_PTR` thread-local is NOT needed — `userdata` is a local variable that survives the `tidepool_sigsetjmp_call` return.

### Step 3: Remove the leak comment

Replace lines 120-123:
```rust
// Old:
if val != 0 {
    // Signal was caught. The closure was interrupted by siglongjmp,
    // so the Box was leaked. We can't recover it safely.
    return Err(SignalError(val));
}

// New:
if val != 0 {
    // Signal was caught. Drop the closure that the trampoline never consumed.
    drop(Box::from_raw(userdata as *mut Box<dyn FnOnce()>));
    return Err(SignalError(val));
}
```

## Verification

```bash
cargo test --workspace
```

The signal_safety tests (if any) should still pass. To verify leak fix: run many evals that trigger SIGILL (e.g., exhausted case branch) and check that RSS stays flat over time.
