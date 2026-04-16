# Public API Documentation Pass

## Status: Queued (after idiom-cleanup merges)

## Problem
Public types/functions across crates lack doc comments. Downstream crate consumers (and `cargo doc`) get no guidance. Coverage varies — `tidepool-codegen` is well-documented (534 doc comments for 65 pub items), but `tidepool-effect` (5 pub items, 35 comments) and `tidepool-bridge` (3 pub items, 47 comments) are thin on API docs despite having narrative comments.

## Scope
Doc comments (`///`) on all `pub` items in library crates. Focus on:
- Type-level docs: what the type represents, when to use it
- Function-level docs: what it does, panics, errors
- Module-level docs (`//!`): purpose of the module

NOT in scope: internal/private items, examples, tutorials.

## Coverage Baseline

| Crate | Pub Items | Doc Comments | Priority |
|-------|-----------|-------------|----------|
| tidepool-repr | 26 | 94 | HIGH — core IR, most consumed |
| tidepool-eval | 15 | 61 | HIGH — Value, Env, eval API |
| tidepool-optimize | 14 | 41 | MED — pass interfaces |
| tidepool-effect | 5 | 35 | MED — small but conceptually dense |
| tidepool-heap | 11 | 97 | LOW — already decent |
| tidepool-runtime | 9 | 78 | MED — user-facing API |
| tidepool-bridge | 3 | 47 | LOW — already decent |
| tidepool-mcp | 25 | 95 | LOW — internal server |
| tidepool-codegen | 65 | 534 | LOW — already well-documented |

## Plan

### Leaf 1: `tidepool-repr` (highest priority)
- `CoreFrame`, `RecursiveTree`, `VarId`, `Literal`, `PrimOpKind`, `DataConTable`
- `serial::read_cbor`, `serial::write_cbor`
- Module-level `//!` docs
- Verify: `cargo doc -p tidepool-repr --no-deps` (no warnings)

### Leaf 2: `tidepool-eval`
- `Value`, `Env`, `eval`, `EvalError`
- `ThunkState`, `Heap` trait
- Verify: `cargo doc -p tidepool-eval --no-deps`

### Leaf 3: `tidepool-optimize` + `tidepool-effect` + `tidepool-runtime`
- Pass entry points: `beta_reduce`, `dead_code_eliminate`, `inline`, `case_reduce`
- `DispatchEffect`, `EffectHandler`
- `compile_haskell`, `compile_and_run`, `CompileError`
- Verify: `cargo doc` per crate

## Verification
```bash
# No doc warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo test --workspace  # ensure no breakage
```

## Boundary
- Doc comments ONLY — no code changes
- No `#[doc(hidden)]` changes
- No README or markdown files
- Preserve existing doc comments verbatim
