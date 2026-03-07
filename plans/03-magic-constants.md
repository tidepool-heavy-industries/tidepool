# Plan: Name magic constants (0x45, 0x1000, lit tags in heap_bridge)

## Goal

Replace raw magic numbers with named constants for readability and single-source-of-truth.

## Changes

### A. Error sentinel tag `0x45`

**Define in:** `tidepool-repr/src/types.rs` (near the top, with other constants)

```rust
/// Tag byte stored in high bits of VarId to mark error-sentinel bindings.
/// These are `error "..."` calls hoisted by GHC into let bindings.
pub const ERROR_SENTINEL_TAG: u8 = 0x45;
```

**Replace in 3 files (5 sites):**

1. `tidepool-eval/src/eval.rs:141` — `if tag == 0x45` → `if tag == tidepool_repr::ERROR_SENTINEL_TAG`
2. `tidepool-codegen/src/emit/expr.rs:226` — `if tag == 0x45` → `if tag == tidepool_repr::ERROR_SENTINEL_TAG`
3. `tidepool-codegen/src/emit/expr.rs:1142` — `(v.0 >> 56) as u8 == 0x45` → `== tidepool_repr::ERROR_SENTINEL_TAG`
4. `tidepool-codegen/src/emit/expr.rs:1154` — `(v.0 >> 56) as u8 == 0x45` → `== tidepool_repr::ERROR_SENTINEL_TAG`
5. Comment at `tidepool-codegen/src/emit/expr.rs:1129` — update to reference the constant name

### B. Pointer validity threshold `0x1000`

**Define in:** `tidepool-codegen/src/host_fns.rs` (near top, with other constants)

```rust
/// Addresses below this are considered invalid (null page guard).
const MIN_VALID_ADDR: u64 = 0x1000;
```

**Replace 9 sites in `tidepool-codegen/src/host_fns.rs`:**
Lines 861, 887, 962, 985, 1140, 1168, 1209, 1233, 2105

Each `(ptr as u64) < 0x1000` → `(ptr as u64) < MIN_VALID_ADDR`

### C. Literal tag constants in heap_bridge.rs

**File:** `tidepool-codegen/src/heap_bridge.rs`

Import existing constants from `crate::emit`:
```rust
use crate::emit::{LIT_TAG_INT, LIT_TAG_WORD, LIT_TAG_CHAR, LIT_TAG_FLOAT,
    LIT_TAG_DOUBLE, LIT_TAG_STRING, LIT_TAG_ADDR, LIT_TAG_BYTEARRAY,
    LIT_TAG_SMALLARRAY, LIT_TAG_ARRAY};
```

Replace raw integers in match arms (lines 92-157) and assignments (lines 229-314).

The match arms use `u8` but the constants are `i64`, so cast: `x if x == LIT_TAG_INT as u8 =>` or define `u8` versions. Simplest: define local `const` aliases as `u8` at top of function, or cast in each arm.

## Verify

```bash
cargo check --workspace
cargo test -p tidepool-codegen -p tidepool-eval
```

## Boundary

- Do NOT change any logic — purely mechanical rename
- Do NOT touch test files
- The `0x1000` constant is local to host_fns.rs (not cross-crate)
- The `ERROR_SENTINEL_TAG` goes in tidepool-repr because both eval and codegen need it
