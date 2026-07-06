# Error Type Consolidation

## Status: Done

Landed incrementally across many prior commits (`refactor(eval): type-driven
error enums`, `refactor(codegen): consolidate YieldError/RuntimeError via
Runtime(#[from] …)`, and others) without ever closing this doc out — by the
time `root.error-consolidation` picked this up, all 16 originally-listed error
enums already derived `thiserror::Error` with faithful `Display` messages, and
the `From` hierarchy (`EmitError -> PipelineError -> JitError`,
`CompileError`/`JitError -> RuntimeError`, `EvalError`/`BridgeError ->
EffectError -> JitError`) was already wired. The `.expect()` hot spots named
below (`serial/mod.rs`, `heap_bridge.rs`, `host_fns.rs`) were also already
clean — zero production `.expect()`/`.unwrap()` remained in any of the three.

Remaining work done in this pass: derived `thiserror::Error` on three error
enums that were missed by the earlier passes (not part of the original
16-enum inventory below, found by an exhaustive re-scan) — `PipelineError`
(`tidepool-optimize`, previously hand-rolled `Display`/`Error`), `TextShapeError`
(`tidepool-eval/src/shapes.rs`, previously undecorated), and `StartError`
(`tidepool-runtime/src/session/engine.rs`, previously undecorated). A full
production-code `.expect()`/`.unwrap()` re-scan (excluding tests, examples,
build scripts, proc-macro expansion, and test-support crates) found no
further genuinely-recoverable panics to convert — see the branch's submit
note for the per-site judgment list.

## Problem (as originally scoped — see Status above for current state)
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
