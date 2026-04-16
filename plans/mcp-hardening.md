# MCP Server Resilience

Two small hardening items left on the MCP server after the permit/eviction refactor.

## Status: Queued

## Orphaned eval thread on timeout

`tidepool-mcp/src/lib.rs:1515` — the eval thread is spawned with `let _handle = std::thread::Builder::new()...spawn(...)`. The `JoinHandle` is dropped immediately. When eval times out, the thread keeps running on an 8-256 MB stack until it naturally exits.

The `eval_semaphore` cap (permit acquired at line 1519 via `let _permit = permit;`) limits **concurrent** orphans: the permit releases only when the thread finally returns. So runaway evals don't accumulate unboundedly — they block new evals instead, which is visible to callers as "Server busy". That's a partial mitigation, not a fix.

### Fix

On timeout, move `handle` into a detached `tokio::task::spawn_blocking` that sleeps a short grace period then `handle.join()`s. Log if join returns `Err`. Optionally track an `Arc<AtomicUsize>` orphan count and reject new evals above a threshold with a clearer message than the generic "server busy".

### Verify

Manually eval `let loop x = loop x in loop 0` — confirm timeout returns cleanly and `ps -T -p $PID` thread count drops after the grace period.

## Residual `.lock().unwrap()` calls

The 2025-era mutex-poisoning cleanup covered most of `tidepool-eval` (0 remaining) but left one site in `tidepool-mcp/src/lib.rs` (find via `rg '\.lock\(\)\.unwrap\(\)' tidepool-mcp`).

### Fix

Convert to `parking_lot::Mutex` (no poisoning, `.lock()` returns guard directly), or handle the `PoisonError` case explicitly. The `parking_lot` route is one-line per site; no behavioral regression risk.

### Verify

```bash
cargo test --workspace
```

## Boundary

- Do not rework the eval concurrency model (semaphore + continuation eviction) — it's load-bearing.
- Do not add `parking_lot` globally; keep the scope to the one file that needs it.
