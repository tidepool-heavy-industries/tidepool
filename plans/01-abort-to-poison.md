# Plan: Replace abort() with runtime_error + poison in host_fns.rs

## Goal

Replace `std::process::abort()` calls in JIT host callbacks with the established `runtime_error()` + `error_poison_ptr()` pattern, so a bad eval doesn't kill the MCP server.

## File

`tidepool-codegen/src/host_fns.rs`

## Context

The codebase already has a recoverable error pattern:
1. `runtime_error(kind)` sets `RUNTIME_ERROR` thread-local and returns `error_poison_ptr()`
2. `error_poison_ptr()` returns a valid-looking closure that the effect machine catches before user code sees it
3. This pattern is used by `runtime_error`, `runtime_oom`, `runtime_blackhole_trap`

There are 18 `abort()` calls. They fall into three categories:

### Category A: Layout failures (7 sites) — KEEP as abort

Lines 581, 635, 846, 921, 944, 1013, 1038

These are `Layout::from_size_align(...).unwrap_or_else(|_| abort())`. The layout computation uses constant alignment (8) and small sizes. If this fails, the process is in an unrecoverable state (memory corruption). Returning a poison pointer after a failed alloc layout is not meaningful. **Leave these as-is.**

### Category B: Pointer validation (10 sites) — CONVERT to poison

Lines 861, 873, 887, 962, 985, 1140, 1168, 1209, 1233, 2105

These check `(ptr as u64) < 0x1000` and abort. A bad pointer in eval should not kill the server. Convert to:
1. Set `RUNTIME_ERROR` to `RuntimeError::Undefined` (or a new `BadPointer` variant)
2. Return `error_poison_ptr()`

For functions returning `i64` (like `runtime_strlen`, `runtime_text_measure_off`, `runtime_text_memchr`): set the error flag and return 0. The effect machine checks the error flag after each step.

### Category C: Case trap (1 site, line 2179) — CONVERT to poison

`runtime_case_trap` should set `RUNTIME_ERROR` and return `error_poison_ptr()` instead of aborting. Keep the diagnostic eprintln — it's useful. Just don't kill the process.

### Category D: CString failure (1 site, line 1279) — CONVERT to poison

`runtime_show_double_addr`: interior null byte in double formatting. Set error and return poison.

## Steps

1. Add a helper function:
```rust
/// Check pointer validity; if bad, set runtime error and return true.
fn check_ptr_invalid(ptr: *const u8, fn_name: &str) -> bool {
    if (ptr as u64) < 0x1000 {
        eprintln!("[BUG] {}: bad pointer {:#x}", fn_name, ptr as u64);
        RUNTIME_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(RuntimeError::Undefined);
        });
        true
    } else {
        false
    }
}
```

2. Replace each Category B site. Example for `runtime_set_byte_array` (line 887):
```rust
// Before:
if (ba as u64) < 0x1000 {
    eprintln!("[BUG] runtime_set_byte_array: ...");
    std::process::abort();
}
// After:
if check_ptr_invalid(ba, "runtime_set_byte_array") { return; }
```

For functions returning `*mut u8`: return `error_poison_ptr()`.
For functions returning `i64`: return `0`.
For functions returning `()` (void): just return.

3. Convert `runtime_case_trap` (line 2179): keep all diagnostic output, replace final `abort()` with setting `RUNTIME_ERROR::Undefined` and returning `error_poison_ptr()`.

4. Convert `runtime_show_double_addr` (line 1279): replace `abort()` with setting error and returning a poison pointer for the CString failure path.

## Affected Lines

- New helper: ~10 lines
- Line 861, 873: `runtime_copy_addr_to_byte_array` — returns `()`
- Line 887: `runtime_set_byte_array` — returns `()`
- Line 962: `runtime_copy_byte_array` — returns `()`
- Line 985: `runtime_compare_byte_arrays` — returns `i64`
- Line 1140: `runtime_strlen` — returns `i64`
- Line 1168: `runtime_text_measure_off` — returns `i64`
- Line 1209: `runtime_text_memchr` — returns `i64`
- Line 1233: `runtime_text_reverse` — returns `()`
- Line 1279: `runtime_show_double_addr` — returns `*mut u8`
- Line 2105, 2179: `runtime_case_trap` — returns `*mut u8`

## Verify

```bash
cargo check -p tidepool-codegen
cargo test -p tidepool-codegen
cargo test -p tidepool-runtime
```

## Boundary

- Do NOT touch Category A (layout failures) — those stay as abort
- Do NOT add new RuntimeError variants — use `Undefined` for all pointer errors
- Do NOT change function signatures (extern "C" ABI)
- Do NOT remove the eprintln diagnostics — they're valuable for debugging
