# Error Type Consolidation

## Status: Queued (after idiom-cleanup merges)

## Problem
16 error enums across 8 crates with manual wrapping, 72 `.expect()` calls in production code, inconsistent `From` impl coverage. Error chains are opaque — `JitError::Compilation(String)` loses the original error type.

## Current Error Landscape

| Crate | Error Types | Notes |
|-------|-------------|-------|
| tidepool-repr | `ReadError`, `WriteError` | CBOR serial |
| tidepool-eval | `EvalError` | Tree-walking eval |
| tidepool-codegen | `JitError`, `PipelineError`, `EmitError`, `YieldError`, `RuntimeError`, `BridgeError`, `HeapError` | 7 types, natural hierarchy |
| tidepool-runtime | `CompileError`, `RuntimeError` | High-level API |
| tidepool-effect | `EffectError` | Effect dispatch |
| tidepool-heap | `GcError`, `HeapError` | GC + arena |
| tidepool-bridge | `BridgeError` | FromCore/ToCore |

### Hot spots (`.expect()` in prod code)
- `serial/mod.rs`: 24 calls (CBOR encode/decode)
- `heap_bridge.rs`: 20 calls (heap operations)
- `host_fns.rs`: 4 calls (runtime host functions)

## Plan

### Leaf 1: `tidepool-repr` errors
- Add `thiserror` to repr's deps
- Derive `thiserror::Error` + `Display` on `ReadError`, `WriteError`
- Convert `.expect()` calls in `serial/mod.rs` to `?` with proper error variants
- Verify: `cargo test -p tidepool-repr && cargo clippy -p tidepool-repr`

### Leaf 2: `tidepool-codegen` error hierarchy
- Add `thiserror` to codegen's deps
- Derive on all 7 error types
- Add `From` impls: `EmitError -> PipelineError -> JitError`
- Convert `.expect()` in `heap_bridge.rs`, `host_fns.rs` to `?`
- Verify: `cargo test -p tidepool-codegen && cargo clippy -p tidepool-codegen`

### Leaf 3: `tidepool-heap` + `tidepool-eval` + `tidepool-effect`
- Derive `thiserror::Error` on `GcError`, `HeapError`, `EvalError`, `EffectError`
- Convert remaining `.expect()` calls
- Verify per-crate tests

### Leaf 4: `tidepool-runtime` + `tidepool-bridge`
- Derive on `CompileError`, `RuntimeError`, `BridgeError`
- Add `From` impls connecting to downstream errors
- Verify per-crate tests

## Verification
```bash
cargo test --workspace
cargo clippy --workspace
```

## Boundary
- No new error variants unless replacing a `.expect()`/`.unwrap()`
- No behavioral changes — same errors, better types
- `thiserror` is the only new dependency
