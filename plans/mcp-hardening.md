# MCP Server Resilience

Two small hardening items left on the MCP server after the permit/eviction refactor.

## Status: CONFIRMED-FIXED (verified 2026-06-11, Wave-7 Agent B)

Both items below were resolved by prior work and verified independently. The
original spec is preserved beneath each verdict as history.

### Verdict summary

| Item | Verdict | Shipped in | Evidence |
|------|---------|------------|----------|
| Orphaned eval thread on timeout | **CONFIRMED-FIXED** (exceeds spec) | `ff07cdd` (#269), hardened by `97c6108` (pause-gate) | `JoinHandle` carried through `EvalSession.thread`; reaped via `reap_detached` with orphan accounting (`orphaned_threads` + `MAX_ORPHANED_EVALS` admission gate); joined on crash for panic forensics. No drop-unjoined path remains. |
| Residual `.lock().unwrap()` | **CONFIRMED-FIXED** | `ff07cdd` (#269) | Zero `.lock().unwrap()` in `tidepool-mcp`. `parking_lot::Mutex` for `PauseGate`/`continuations`. One `std::sync::Mutex` (`CapturedOutput`) with explicit poison-recovery — later converted to `parking_lot` for consistency in the idiom pass. |

**Test coverage:** `test_pause_gate_park_resume_abort` (gate state machine),
`test_timeout_parks_paused_continuation_and_resume_collects` (full
timeout→park→resume lifecycle), `test_eval_orphaned_overload` (admission gate).

#### Verification notes — orphan lifecycle (`tidepool-mcp/src/lib.rs`)

The fix is structurally different from (and stronger than) the spec below. Rather
than always detaching on timeout, **timeout became a yield point**: the eval
thread parks at its next effect boundary (`PauseGate::checkpoint`, ~1548) and the
caller receives a continuation. The thread handle is carried across park/resume
cycles in `EvalSession.thread` (~1636) and reaped on every terminal path:

- **Timeout → parked:** `thread: handle.take()` stored in the continuation
  (~1923); reaped later by resume-completion, abort, or pressure eviction.
- **Timeout → pure-compute runaway** (no effect within grace): `reap_detached`
  (~1945, ~2130) — background grace-then-join with `orphaned_threads`
  accounting; `eval()` refuses new work above `MAX_ORPHANED_EVALS` (~2147).
- **Crash (`None` message):** `handle.join()` for panic-payload forensics (~2078).
- **Eviction / abort:** `reap_detached(session.thread)` (~1849) / handle threaded
  back through `handle_session_result` (~2487).
- **Completed / Error:** handle dropped — benign, the thread has already sent its
  terminal message and is exiting.

The spec's "optional `Arc<AtomicUsize>` orphan count" is implemented exactly as
`orphaned_threads` with a clearer-than-"server busy" overload message.

#### Verification notes — `.lock().unwrap()`

`rg '\.lock\(\)\.unwrap\(\)' tidepool-mcp/src` → 0 matches. The two mutex sites
are both poison-safe by construction: `PauseGate` uses `parking_lot::Mutex` (no
poisoning), and `CapturedOutput` used `std::sync::Mutex` with
`unwrap_or_else(|e| e.into_inner())` recovery — converted to `parking_lot` in the
idiom pass since `parking_lot::Mutex` was already imported in the same file.

### Adjacent verify-first findings (Wave-7)

- **`tidepool-runtime/src` unwraps:** all 42 are inside `#[cfg(test)]` modules
  (`render.rs` ≥549, `cache.rs` ≥369, `lib.rs` 275). Zero on eval-serving paths.
  The `Value::ByteArray` `Arc<Mutex<Vec<u8>>>` lock sites in `render.rs` already
  use `unwrap_or_else(|e| e.into_inner())` poison recovery. No action needed.
- **`tidepool/src/main.rs` handler arms:** all 15 `panic!` + 101 `.unwrap()` + 5
  `.expect()` are inside `#[cfg(test)] mod tests` (>1818). The effect handlers
  (`Console`/`Kv`/`Fs`/`Sg`/...) already return `Result<Response, EffectError>`
  via `.map_err(EffectError::Handler)`, so malformed effect data arrives as a
  clean, `try*`-catchable effect error — the design intent. No data-reachable
  panic to convert. No action needed.

---

## Original spec (history)

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
