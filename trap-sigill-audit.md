# Audit: Cranelift Trap Propagation in Tidepool

## 1. Why do traps manifest as SIGILL?

Cranelift's `trap` instruction is designed to generate a target-specific trap. On x86_64, this compiles to the `ud2` instruction. When the CPU encounters `ud2`, it raises an "Invalid Opcode" exception, which the operating system translates into a `SIGILL` signal sent to the process.

Because the Tidepool JIT machine currently executes the compiled code as a direct `extern "C"` function call without any signal handler or supervisor context, the `SIGILL` signal is unhandled, leading to immediate process termination by the OS.

## 2. Current Trap Locations

The audit identified the following locations where `trap` is used:

- **`tidepool-codegen/src/alloc.rs:129`**: `TrapCode::unwrap_user(1)` is used when a heap allocation fails even after a GC cycle (Heap Overflow).
- **`tidepool-codegen/src/emit/case.rs:80, 169, 279`**: `TrapCode::unwrap_user(2)` is used when a `case` expression has no matching alternative and no default branch (Non-exhaustive Pattern Match).

## 3. Propagation and Recovery

Currently, there is no mechanism to catch or propagate these traps as Rust errors:
- `jit_machine.rs` calls the function pointer directly.
- `std::panic::catch_unwind` is NOT used, and would be ineffective anyway as it only catches Rust panics (unwinding), not hardware signals like `SIGILL`.
- Cranelift does not automatically provide a signal handler; it is the responsibility of the embedder (Tidepool) to provide one if traps are to be caught.

## 4. Recommendations

### (a) Correct way to handle traps
In the current Tidepool architecture, the "correct" way to handle errors is through the established `runtime_error` mechanism in `host_fns.rs`. This mechanism uses thread-local storage to flag errors and returns a "poison" heap object to allow the JIT code to return gracefully to the host.

### (b) Strategy for `run_pure` and `run`
Instead of installing a complex signal handler (which requires cross-platform `sigaction`/`SEH` and `siglongjmp`), Tidepool should replace hardware traps with software error reporting. 

`std::panic::catch_unwind` should be avoided for JIT code as it assumes Rust-compatible unwinding metadata which may not be fully present or correct for JIT frames (though Tidepool does preserve frame pointers).

### (c) Strategy for `alloc.rs` and `case.rs`
The `trap` instructions should be replaced with:
1. A call to the `runtime_error` host function with a specific error kind.
2. A return from the JIT function using the `error_poison_ptr()` as the result.

### (d) Proposed `RuntimeError` additions
Add the following variants to `RuntimeError` in `host_fns.rs`:
- `HeapOverflow` (replacing `unwrap_user(1)`)
- `NonExhaustiveMatch` (replacing `unwrap_user(2)`)

## 5. Verification
The findings in this audit explain why tests hitting these paths (e.g., exhaustion of the nursery or incomplete case expressions) crash the test runner with "illegal hardware instruction" instead of returning a `Result::Err`.
