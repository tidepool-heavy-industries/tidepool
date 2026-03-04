# Plan 1: Orphaned eval threads on timeout

## Problem

`tidepool-mcp/src/lib.rs:1367` spawns eval threads with `let _handle = std::thread::Builder::new()...spawn(...)`. The `JoinHandle` is immediately dropped (underscore prefix). When eval times out at line 1486, the thread keeps running. Each zombie thread uses ~8MB stack (`stack_size(8 * 1024 * 1024)` at line 1369). Over a long MCP session with many timeouts (infinite loops, hung commands), threads accumulate → OOM or OS thread exhaustion.

## Files to modify

- `tidepool-mcp/src/lib.rs`

## Implementation

### Step 1: Add cancellation flag to `TidepoolMcpServerImpl`

Add a field to track active orphan count:

```rust
// In TidepoolMcpServerImpl struct (line 1301):
orphaned_threads: Arc<AtomicUsize>,
```

Initialize to 0 in the constructor.

### Step 2: Track thread handle, attempt join on timeout

In `eval()` (line 1367), change `let _handle = ...` to `let handle = ...`.

On the timeout path (line 1486-1496), after returning the timeout error, spawn a background task to join the thread:

```rust
Err(_elapsed) => {
    tracing::error!("eval timed out after {}s", EVAL_TIMEOUT_SECS);
    let orphan_count = Arc::clone(&self.orphaned_threads);
    orphan_count.fetch_add(1, Ordering::Relaxed);
    tokio::task::spawn_blocking(move || {
        // Give the thread 2s to finish naturally
        std::thread::sleep(Duration::from_secs(2));
        match handle.join() {
            Ok(_) => { orphan_count.fetch_sub(1, Ordering::Relaxed); }
            Err(_) => { tracing::warn!("eval thread could not be joined"); }
        }
    });
    // ... existing error response ...
}
```

### Step 3: Reject evals when too many orphans

At the top of `eval()`, check the orphan count:

```rust
let orphans = self.orphaned_threads.load(Ordering::Relaxed);
if orphans >= 10 {
    return Ok(CallToolResult::error(vec![Content::text(
        "Server overloaded: too many timed-out evaluations still running. Please wait."
    )]));
}
```

### Step 4: Handle the `handle` move into the timeout branch

The `handle` is created before the `match timeout(...)` block. Since `handle` is only needed in the timeout branch, wrap it in an `Option` and take it in that branch. Or restructure so `handle` is consumed appropriately. The simplest approach: `handle` must be moved into the `spawn_blocking` closure only on timeout — use `let handle = Arc::new(Mutex::new(Some(handle)))` and take from it in the timeout path.

Actually simpler: the `handle` variable is defined before the match, and each match arm consumes it differently. Since only the timeout arm needs it, and Rust's borrow checker won't let us move it into just one arm, store it as `Option` and take in the timeout arm.

## Verification

```bash
cargo test --workspace
```

Manual test: eval `let loop x = loop x in loop 0` via MCP — should timeout after 120s. Check `ps -T -p $PID | wc -l` before and after to verify thread cleanup.
