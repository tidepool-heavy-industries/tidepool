# Plan 5: Handle mutex poisoning gracefully

## Problem

The MCP server and eval interpreter use `std::sync::Mutex` with `.lock().unwrap()` everywhere. If any thread panics while holding a lock, the mutex is poisoned and ALL subsequent `.lock().unwrap()` calls panic — cascading failure. The server becomes permanently broken until restart.

### Affected locations

**tidepool-mcp/src/lib.rs** (continuation HashMap + CapturedOutput):
- Line 1171: `self.lines.lock().unwrap().push(line);` (CapturedOutput::push)
- Line 1176: `self.lines.lock().unwrap()` (CapturedOutput::drain)
- Line 1311: `continuations: Arc<std::sync::Mutex<HashMap<...>>>` (field)
- Line 1322: `self.continuations.lock().unwrap()` (cleanup_stale_continuations)
- Line 1459: `self.continuations.lock().unwrap().insert(...)` (eval — store continuation)
- Line 1552: `self.continuations.lock().unwrap().insert(...)` (resume — store continuation)

**tidepool-eval/src/eval.rs** (ByteArray mutex — `Arc<Mutex<Vec<u8>>>`):
- 15+ locations using `.lock().unwrap()` on ByteArray fields in primop handlers

## Files to modify

- `tidepool-mcp/Cargo.toml` — add `parking_lot` dependency
- `tidepool-mcp/src/lib.rs` — replace `std::sync::Mutex` with `parking_lot::Mutex`
- `tidepool-eval/Cargo.toml` — add `parking_lot` dependency
- `tidepool-eval/src/eval.rs` — replace ByteArray Mutex
- `tidepool-eval/src/value.rs` — ByteArray type definition uses `Arc<Mutex<Vec<u8>>>`

## Implementation

### Approach: Use `parking_lot::Mutex` (no poisoning)

`parking_lot::Mutex` never poisons — `.lock()` returns the guard directly (not `Result`). This eliminates all `.unwrap()` calls on locks.

### Step 1: Add parking_lot to both crates

`tidepool-mcp/Cargo.toml`:
```toml
parking_lot = "0.12"
```

`tidepool-eval/Cargo.toml`:
```toml
parking_lot = "0.12"
```

### Step 2: Replace in tidepool-mcp/src/lib.rs

1. Change the import: `use std::sync::Mutex;` → `use parking_lot::Mutex;`
2. The `continuations` field at line 1311: `Arc<std::sync::Mutex<HashMap<...>>>` → `Arc<parking_lot::Mutex<HashMap<...>>>`
3. `CapturedOutput` at line 1161: `Arc<std::sync::Mutex<Vec<String>>>` → `Arc<parking_lot::Mutex<Vec<String>>>`
4. Remove all `.unwrap()` after `.lock()` — parking_lot's `.lock()` returns `MutexGuard` directly

### Step 3: Replace in tidepool-eval

1. Find the ByteArray type definition (likely in `tidepool-eval/src/value.rs`) — it uses `Arc<Mutex<Vec<u8>>>`
2. Change to `Arc<parking_lot::Mutex<Vec<u8>>>`
3. Remove all `.lock().unwrap()` → `.lock()` in eval.rs primop handlers

### Step 4: Check other crates for Mutex usage

Search for `std::sync::Mutex` in the workspace to catch any other uses. The `tidepool-runtime/src/render.rs` also has ByteArray `.lock().unwrap()` calls — those use the same `Value` type from `tidepool-eval`, so fixing the type definition fixes render.rs too.

## Verification

```bash
cargo test --workspace
```

All tests pass. The behavioral change is: a panic while holding a lock no longer permanently breaks the server.
