# Plan: MCP eval/resume dedup + miscellaneous fixes

## Goal

Small fixes across the codebase that don't warrant their own leaf.

## Changes

### A. Extract `handle_session_message` in MCP server

**File:** `tidepool-mcp/src/lib.rs`

The `eval` method (lines 1591-1664) and `resume` method (lines 1702-1775) have identical 5-arm match blocks on `SessionMessage`. Extract a shared helper:

```rust
async fn handle_session_message(
    &self,
    rx: &mut tokio::sync::mpsc::Receiver<SessionMessage>,
    eval_timeout: Duration,
    session_id: Option<String>,  // None for eval (generate new), Some for resume
) -> Result<CallToolResult, McpError> {
    match timeout(eval_timeout, rx.recv()).await {
        Ok(Some(SessionMessage::Completed { result, output })) => { ... }
        Ok(Some(SessionMessage::Suspended { prompt })) => { ... }
        Ok(Some(SessionMessage::Error { error })) => { ... }
        Ok(None) => { /* crash.log reading */ ... }
        Err(_) => { /* timeout */ ... }
    }
}
```

Both `eval` and `resume` call this after setting up the session.

### B. Fix nursery size doc comment

**File:** `tidepool-runtime/src/lib.rs:270`

```rust
// Before:
/// using the default nursery size (1 MiB).
// After:
/// using the default nursery size (64 MiB).
```

### C. Relax schemars version pin

**File:** `tidepool-mcp/Cargo.toml:25`

```toml
# Before:
schemars = "=1.2.1"
# After:
schemars = "1.2"
```

### D. Remove heap_alloc placeholder

**File:** `tidepool-codegen/src/host_fns.rs`

Lines 293-296: `heap_alloc` always returns `null_mut()`. Remove the function and its registration at line 1358 in `host_fn_symbols`. If anything calls it, the linker will tell us.

### E. Remove debug eprintln in LLM handler

**File:** `tidepool/src/main.rs:1301-1306`

Remove or convert to `tracing::debug!`:
```rust
eprintln!("[llm-structured] API response input: ...");
eprintln!("[llm-structured] cx.respond result: ...");
```

## Verify

```bash
cargo check --workspace
cargo test -p tidepool-mcp -p tidepool-codegen -p tidepool-runtime
```

## Boundary

- Do NOT change any eval/resume logic beyond extracting the shared match
- Do NOT change schemars API usage, just the version constraint
- For heap_alloc removal: if it's referenced by JIT-compiled code (not just the symbol table), leave it and add a TODO instead
