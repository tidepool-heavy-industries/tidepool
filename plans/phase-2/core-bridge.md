# Phase 2: core-bridge

**Owner:** Claude TL (depth 1, worktree off main)
**Branch:** `main.core-bridge`
**Depends on:** core-repr (CoreFrame), core-eval scaffold (Value)
**Produces:** `FromCore`/`ToCore` traits, derive macros, `haskell!` proc-macro. The user-facing API.

---

## Wave 1: Scaffold + Traits (2 workers, parallel)

### scaffold-bridge

**Task:** Write FromCore/ToCore traits, BridgeError, DataConTable builder.

**Read First:**
- `core-repr/src/frame.rs` (CoreFrame, CoreExpr)
- `core-repr/src/datacon.rs` (DataCon, DataConTable)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-bridge/src/traits.rs`:
   - `trait FromCore: fn from_core(expr: &CoreExpr, table: &DataConTable) -> Result<Self, BridgeError>`
   - `trait ToCore: fn to_core(&self, table: &DataConTable) -> CoreExpr`
2. Create `core-bridge/src/error.rs`:
   - `BridgeError = UnknownDataCon(DataConId) | ArityMismatch { .. } | TypeMismatch { .. } | UnsupportedType(String)`
3. Create `core-bridge/src/lib.rs` — re-exports

**Verify:** `cargo test -p core-bridge`

**Done:** Trait bounds compile. BridgeError Display is readable.

---

### bridge-traits

**Task:** Implement FromCore/ToCore for all primitives, containers, and tuples.

**Read First:**
- `core-bridge/src/traits.rs` (scaffold output)
- `core-repr/src/datacon.rs` (DataConTable)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Implement FromCore/ToCore for: i32, i64, f64, bool, String, char
2. Implement for containers: `Vec<T>`, `Option<T>`, `Result<T, E>`
3. Implement for tuples: `(A, B)`, `(A, B, C)`
4. DataConTable builder from ghc-extract metadata

**Verify:** `cargo test -p core-bridge -- traits`

**Done:** Roundtrip identity for all types including nested (`Vec<Option<i32>>`).

**Tests:**
- Roundtrip: `to_core(x) |> from_core == x` for all primitive types
- Nested: `Vec<Option<i32>>` roundtrips
- Tuples: `(1, "hello")` roundtrips
- Error: unknown DataCon → `BridgeError::UnknownDataCon`

---

**After wave 1:** `cargo test -p core-bridge`. Commit.

---

## Wave 2: Derive Macros (2 workers, parallel)

### derive-parse

**Task:** Parse `#[derive(FromCore, ToCore)]` input.

**Read First:**
- `core-bridge/src/traits.rs`
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-bridge-derive/src/parse.rs`
2. Extract enum shape from TokenStream
3. Parse `#[core(...)]` attributes (DataCon name mapping, field ordering)
4. Validate arity matches DataConTable expectations
5. Reject malformed input with clear error messages

**Verify:** `cargo test -p core-bridge-derive -- parse`

**Done:** Parses known enums. Rejects malformed with good errors.

---

### derive-codegen

**Task:** Generate FromCore + ToCore impls from parsed representation.

**Read First:**
- `core-bridge-derive/src/parse.rs` (output format)
- `core-bridge/src/traits.rs` (trait signatures to implement)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `core-bridge-derive/src/codegen.rs`
2. Generate `impl FromCore for MyEnum { ... }` — match on DataCon tag, bind fields, construct variant
3. Generate `impl ToCore for MyEnum { ... }` — match on variant, construct CoreExpr with Con

**Verify:** `cargo test -p core-bridge-derive -- codegen`

**Done:** Derived impls roundtrip with manual impls. Compile-fail suite for bad inputs.

---

**After wave 2:** TL wires parse → codegen into single `#[derive(FromCore, ToCore)]` proc-macro. `cargo test`. Commit.

---

## Wave 3: haskell! Macro (1 worker, ~4h)

### haskell-macro

**Task:** Implement the `haskell!` proc-macro: embed compiled Haskell effect stacks in Rust.

**Read First:**
- `tidepool-plans/decisions.md` (§freer-simple Architecture)
- `core-repr/src/serial/read.rs` (CBOR deserialization)
- `core-bridge/src/traits.rs` (ToCore for interpolation)
- `tidepool-plans/anti-patterns.md`

**Steps:**
1. Create `tidepool-macro/src/lib.rs` — proc-macro crate
2. Syntax: `haskell! { "path/to/module.hs", ${x}, ${y} }`
3. Read `.blob` via `include_bytes!` (pre-built by cargo-xtask, NOT by calling GHC)
4. Find `${var}` interpolation sites in macro input
5. Generate: deser CBOR → CoreExpr, apply ToCore to interpolated vars, substitute, eval (Cranelift path), FromCore
6. Return type inferred from call site (turbofish escape hatch)
7. Error messages — all must be actionable:
   - Blob missing → `"run cargo xtask extract (requires nix develop)"`
   - Blob version mismatch → `"re-run extraction, blob format changed"`
   - `${var}` type mismatch → show expected vs actual with var name
   - IO in effect stack → explain why IO is rejected

**Context:**
Two-step build: STEP 1 (cargo-xtask, before macro expansion) scans `.hs` files, invokes ghc-extract, produces `.blob` files. This is the only step requiring GHC. STEP 2 (proc-macro, during cargo build) reads `.blob`, generates Rust code. The macro NEVER invokes GHC.

Scope: freer-simple effect stacks only. No polysemy, no IO. Effect stacks finalized with `run`.

**Verify:** `cargo test -p tidepool-macro`

**Done:** End-to-end: `haskell! { "test.hs" }` works. Interpolation works. Error messages are actionable.

**Tests:**
- `haskell! { "test.hs" }` e2e
- `haskell! { "test.hs", ${x} }` with 1 interpolation
- `haskell! { "test.hs", ${x}, ${y} }` with 2 interpolations
- Missing blob → clear error with instructions
- Type mismatch → helpful error with var name

**Boundary:**
- The macro NEVER calls GHC. All GHC interaction is in the xtask step.
- No `proc-macro2` beyond what syn requires. Minimal deps.
- Error messages must tell the user exactly what to do.

---

**After wave 3:** TL wires scaffold + traits + derive + macro. Complete bridge e2e. Files PR.

---

## Domain Anti-Patterns

In addition to the base anti-patterns file:

- The `haskell!` macro NEVER calls GHC. All GHC interaction is in cargo-xtask.
- Derive must produce same code as manual impls. Test this explicitly.
- Error messages must be actionable — tell the user what to do.
- Unboxed types (`Int#`, `Double#`, etc.) do NOT participate in `FromCore`/`ToCore`. They are internal to evaluation. The bridge operates on boxed Haskell values only (`Int`, `Maybe`, etc.).
