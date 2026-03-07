# Plan: Escalate GC forwarding failure from eprintln to panic

## Goal

A missed forwarding-table lookup means an object was reachable but not traced — a fundamental GC invariant violation. Continuing with a stale ThunkId produces a dangling reference that will manifest as a corrupt value far from the root cause. Make this fail loudly.

## File

`tidepool-heap/src/gc/compact.rs`

## Change

**Line 74-80:**

```rust
// Before:
Value::ThunkRef(id) => Value::ThunkRef(table.lookup(*id).unwrap_or_else(|_| {
    eprintln!(
        "GC compact: ThunkRef({}) not in forwarding table — GC trace bug, keeping original",
        id.0
    );
    *id
})),

// After:
Value::ThunkRef(id) => Value::ThunkRef(table.lookup(*id).unwrap_or_else(|_| {
    panic!(
        "GC compact: ThunkRef({}) not in forwarding table — GC trace bug",
        id.0
    );
})),
```

This is one line: change `eprintln!` + `*id` to `panic!`.

## Rationale

- A stale ThunkId after compaction points to deallocated or reused memory
- The current silent fallback makes GC bugs latent and nearly impossible to diagnose
- panic! in the GC is appropriate — if the GC invariant is violated, no heap operation is safe
- This matches the general Rust convention: invariant violations panic, expected errors return Result

## Verify

```bash
cargo check -p tidepool-heap
cargo test -p tidepool-heap
```

## Boundary

- Do NOT change any other GC code
- Do NOT add cfg(debug_assertions) gating — this should panic in release too
- This is a 1-line change
